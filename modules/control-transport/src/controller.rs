use crate::{model::*, stream::Stream};
use amaterasu_media::{ControlMessage, ControlMessageSink, MEDIA_CLOSED};
use amaterasu_module_sdk::{abi::Status, network::Network};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

pub struct Reply {
    pub payload: Value,
    pub created: Option<u64>,
}

struct OperationRecord {
    stream: u64,
    specification: Open,
}

struct RetiredStream {
    owner: String,
    operation_id: Option<String>,
}

pub struct Controller {
    network: Network,
    settings: Settings,
    streams: BTreeMap<u64, Stream>,
    operations: BTreeMap<(String, String), OperationRecord>,
    issued_ids: BTreeSet<u64>,
    retired_streams: BTreeMap<u64, RetiredStream>,
    retirement_epoch: Option<String>,
    used_keys: Vec<[u8; 16]>,
    epoch: Instant,
}

impl Controller {
    #[cfg(test)]
    pub fn new(settings: Settings) -> Self {
        Self::with_network(settings, Network::test_provider())
    }

    pub fn with_network(settings: Settings, network: Network) -> Self {
        Self {
            network,
            settings,
            streams: BTreeMap::new(),
            operations: BTreeMap::new(),
            issued_ids: BTreeSet::new(),
            retired_streams: BTreeMap::new(),
            retirement_epoch: random_epoch(),
            used_keys: Vec::new(),
            epoch: Instant::now(),
        }
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn stage_feedback_for_test(
        &mut self,
        owner: &str,
        id: u64,
        destination: std::net::SocketAddr,
    ) -> ControlResult<()> {
        if !destination.ip().is_loopback() {
            return Err("invalid_destination");
        }
        let stream = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .ok_or("stream_not_found")?;
        stream.stage_pending_datagram_for_test(destination, b"queued-feedback".to_vec());
        stream.stage_reliable_enet_for_test(destination, 17);
        Ok(())
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn pause_local_send_for_test(
        &mut self,
        owner: &str,
        id: u64,
        entered: std::sync::mpsc::SyncSender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> ControlResult<()> {
        let stream = self
            .streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .ok_or("stream_not_found")?;
        stream.pause_local_send_for_test(entered, resume);
        Ok(())
    }

    pub fn handle(&mut self, owner: &str, operation: Operation) -> ControlResult<Reply> {
        let (payload, created) = match operation {
            Operation::Open(request) => return self.open(owner, request),
            Operation::Close { stream } => (self.close(owner, stream)?, None),
            Operation::Retire {
                stream,
                operation_id,
                provider_epoch,
            } => (
                self.retire(owner, stream, &operation_id, &provider_epoch)?,
                None,
            ),
            Operation::RetirementStatus {
                stream,
                operation_id,
                provider_epoch,
            } => (
                self.retirement_status(owner, stream, &operation_id, &provider_epoch)?,
                None,
            ),
            Operation::Lookup { operation_id } => {
                if !valid_name(&operation_id) {
                    return Err("invalid_operation_id");
                }
                let record = self
                    .operations
                    .get(&(owner.into(), operation_id))
                    .ok_or("stream_not_found")?;
                (
                    self.metadata(self.owned(owner, record.stream)?, record.stream),
                    None,
                )
            }
            Operation::Status { stream } => (self.status(owner, stream), None),
        };
        Ok(Reply { payload, created })
    }

    fn open(&mut self, owner: &str, request: Open) -> ControlResult<Reply> {
        if !self.settings.enabled {
            return Err("disabled");
        }
        if !self
            .settings
            .allowed_principals
            .iter()
            .any(|allowed| allowed == owner)
        {
            return Err("forbidden");
        }
        let key = request.validate(self.settings.allow_remote)?;
        if let Some(operation) = &request.operation_id {
            if let Some(record) = self.operations.get(&(owner.into(), operation.clone())) {
                if record.specification != request {
                    return Err("operation_conflict");
                }
                let stream = self.streams.get(&record.stream).ok_or("stream_closed")?;
                return Ok(Reply {
                    payload: self.metadata(stream, record.stream),
                    created: None,
                });
            }
            if self.operations.len() >= 128 {
                return Err("operation_limit");
            }
        }
        self.admit(owner, &key)?;
        let id = self.new_id()?;
        let stream = Stream::open_with_network(&self.network, owner, &request, &key, self.now())?;
        let payload = self.metadata(&stream, id);
        self.streams.insert(id, stream);
        self.issued_ids.insert(id);
        self.used_keys.push(key);
        if let Some(operation) = &request.operation_id {
            self.operations.insert(
                (owner.into(), operation.clone()),
                OperationRecord {
                    stream: id,
                    specification: request,
                },
            );
        }
        Ok(Reply {
            payload,
            created: Some(id),
        })
    }

    fn admit(&self, owner: &str, key: &[u8; 16]) -> ControlResult<()> {
        if self.streams.len() >= 4
            || self
                .streams
                .values()
                .filter(|stream| stream.owner == owner)
                .count()
                >= 2
        {
            return Err("stream_limit");
        }
        if self.issued_ids.len() >= 1024 {
            return Err("stream_history_limit");
        }
        if self.used_keys.contains(key) {
            return Err("key_reuse_forbidden");
        }
        if self.used_keys.len() >= 128 {
            return Err("key_history_limit");
        }
        Ok(())
    }

    fn new_id(&self) -> ControlResult<u64> {
        for _ in 0..16 {
            let mut bytes = [0; 8];
            getrandom::fill(&mut bytes).map_err(|_| "randomness_unavailable")?;
            let id = u64::from_ne_bytes(bytes);
            if id != 0 && !self.issued_ids.contains(&id) {
                return Ok(id);
            }
        }
        Err("randomness_unavailable")
    }

    fn owned(&self, owner: &str, id: u64) -> ControlResult<&Stream> {
        self.streams
            .get(&id)
            .filter(|stream| stream.owner == owner)
            .ok_or("stream_not_found")
    }

    fn owned_mut(&mut self, owner: &str, id: u64) -> Option<&mut Stream> {
        self.streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
    }

    pub fn close(&mut self, owner: &str, id: u64) -> ControlResult<Value> {
        self.owned(owner, id)?;
        // Drop peer, queued datagrams and the owned Network SDK socket before
        // publishing the tombstone. Native calls and tick share this controller.
        drop(self.streams.remove(&id));
        self.retired_streams.insert(
            id,
            RetiredStream {
                owner: owner.into(),
                operation_id: None,
            },
        );
        Ok(json!({"ok": true}))
    }

    fn check_retirement_epoch(&self, expected: &str) -> ControlResult<()> {
        match self.retirement_epoch.as_deref() {
            Some(epoch) if epoch == expected => Ok(()),
            Some(_) => Err("retirement_epoch_mismatch"),
            None => Err("retirement_unavailable"),
        }
    }

    fn retire(
        &mut self,
        owner: &str,
        id: u64,
        operation: &str,
        epoch: &str,
    ) -> ControlResult<Value> {
        self.check_retirement_epoch(epoch)?;
        if !valid_name(operation) {
            return Err("invalid_operation_id");
        }
        if self.retired_streams.iter().any(|(other, record)| {
            *other != id
                && record.owner == owner
                && record.operation_id.as_deref() == Some(operation)
        }) {
            return Err("operation_conflict");
        }
        if let Some(record) = self.retired_streams.get(&id) {
            if record.owner != owner {
                return Err("stream_not_found");
            }
            if record
                .operation_id
                .as_deref()
                .is_some_and(|existing| existing != operation)
            {
                return Err("operation_conflict");
            }
        } else {
            self.close(owner, id)?;
        }
        self.retired_streams
            .get_mut(&id)
            .expect("owned stream is retired")
            .operation_id = Some(operation.into());
        Ok(retirement_reply(id, operation, epoch))
    }

    fn retirement_status(
        &self,
        owner: &str,
        id: u64,
        operation: &str,
        epoch: &str,
    ) -> ControlResult<Value> {
        self.check_retirement_epoch(epoch)?;
        if !valid_name(operation) {
            return Err("invalid_operation_id");
        }
        self.retired_streams
            .get(&id)
            .filter(|record| {
                record.owner == owner && record.operation_id.as_deref() == Some(operation)
            })
            .ok_or("retirement_unknown")?;
        Ok(retirement_reply(id, operation, epoch))
    }

    fn metadata(&self, stream: &Stream, id: u64) -> Value {
        let mut payload = stream.metadata(id);
        payload["retirement_epoch"] = json!(self.retirement_epoch);
        payload
    }

    fn status(&self, owner: &str, requested: Option<u64>) -> Value {
        let streams: Vec<_> = self
            .streams
            .iter()
            .filter(|(id, stream)| {
                stream.owner == owner && requested.is_none_or(|requested| requested == **id)
            })
            .map(|(id, stream)| self.metadata(stream, *id))
            .collect();
        json!({"ok": true, "enabled": self.settings.enabled, "streams": streams})
    }

    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u64::MAX.into()) as u64
    }

    pub fn tick(&mut self) {
        let now = self.now();
        for stream in self.streams.values_mut() {
            stream.tick(now);
        }
    }

    pub fn shutdown(&mut self) {
        self.settings.enabled = false;
        self.streams.clear();
    }

    /// # Safety
    /// Message header and its immutable initialized allocation remain live until return.
    pub unsafe fn send(&mut self, owner: &str, id: u64, message: &ControlMessage) -> Status {
        let now = self.now();
        match self.owned_mut(owner, id) {
            // SAFETY: allocation obligations are forwarded to the owned stream.
            Some(stream) => unsafe { stream.send(message, now) },
            None => MEDIA_CLOSED,
        }
    }

    /// # Safety
    /// Sink context and function remain live for the synchronous callback.
    pub unsafe fn receive(&mut self, owner: &str, id: u64, sink: ControlMessageSink) -> Status {
        match self.owned_mut(owner, id) {
            // SAFETY: sink is borrowed only for this synchronous call.
            Some(stream) => unsafe { stream.receive(sink) },
            None => MEDIA_CLOSED,
        }
    }
}

fn random_epoch() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn retirement_reply(stream: u64, operation_id: &str, provider_epoch: &str) -> Value {
    json!({"ok":true,"state":"retired","scope":"whole_control_stream",
        "stream":stream,"operation_id":operation_id,"provider_epoch":provider_epoch})
}

#[cfg(test)]
pub(crate) mod tests;
