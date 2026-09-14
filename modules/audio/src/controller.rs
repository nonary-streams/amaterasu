use crate::{
    model::*,
    worker::{Backend, NativeBackend, Source},
};
use amaterasu_media::{AudioFrame, AudioSink, MEDIA_BACKEND_ERROR, MEDIA_CLOSED, MEDIA_PENDING};
use amaterasu_module_sdk::abi::{STATUS_OK, Status};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Stream {
    owner: String,
    requested_endpoint: String,
    endpoint: String,
    format: Format,
    source: Box<dyn Source>,
    pending: Option<Packet>,
    failure: Option<&'static str>,
}

struct OperationRecord {
    stream: u64,
    endpoint: String,
}

pub struct Controller {
    backend: Box<dyn Backend>,
    enabled: bool,
    next_stream: u64,
    streams: BTreeMap<u64, Stream>,
    operations: BTreeMap<(String, String), OperationRecord>,
}

pub struct ControlReply {
    pub payload: Value,
    pub created: Option<u64>,
}

impl Controller {
    pub fn new(enabled: bool) -> Self {
        Self {
            backend: Box::new(NativeBackend),
            enabled,
            next_stream: 0,
            streams: BTreeMap::new(),
            operations: BTreeMap::new(),
        }
    }

    pub fn handle(&mut self, owner: &str, operation: Operation) -> AudioResult<ControlReply> {
        let previous = self.next_stream;
        let payload = match operation {
            Operation::Enumerate => {
                let endpoints = if self.enabled {
                    self.backend.enumerate()?
                } else {
                    Vec::new()
                };
                let reply = json!({"ok":true,"enabled":self.enabled,"endpoints":endpoints});
                if serde_json::to_vec(&reply)
                    .map_err(|_| "endpoint_metadata_failed")?
                    .len()
                    > 12_000
                {
                    return Err("endpoint_metadata_limit");
                }
                reply
            }
            Operation::Open {
                endpoint,
                operation_id,
            } => self.open(owner, endpoint, operation_id)?,
            Operation::Lookup { operation_id } => self.lookup(owner, &operation_id)?,
            Operation::Close { stream } => self.close(owner, stream)?,
            Operation::Status { stream } => self.status(owner, stream),
        };
        Ok(ControlReply {
            payload,
            created: (self.next_stream != previous).then_some(self.next_stream),
        })
    }

    fn open(
        &mut self,
        owner: &str,
        endpoint: String,
        operation: Option<String>,
    ) -> AudioResult<Value> {
        if !self.enabled {
            return Err("disabled");
        }
        validate_endpoint(&endpoint)?;
        if let Some(operation) = &operation {
            validate_operation(operation)?;
            if let Some(record) = self.operations.get(&(owner.into(), operation.clone())) {
                if record.endpoint != endpoint {
                    return Err("operation_conflict");
                }
                let stream = self.streams.get(&record.stream).ok_or("stream_closed")?;
                return Ok(open_reply(record.stream, stream));
            }
            if self.operations.len() >= 128 {
                return Err("operation_limit");
            }
        }
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
        let opened = self.backend.open(&endpoint)?;
        let id = self.insert(
            owner,
            endpoint.clone(),
            opened.endpoint,
            opened.format,
            opened.source,
        )?;
        if let Some(operation) = operation {
            self.operations.insert(
                (owner.into(), operation),
                OperationRecord {
                    stream: id,
                    endpoint,
                },
            );
        }
        Ok(open_reply(id, &self.streams[&id]))
    }

    fn insert(
        &mut self,
        owner: &str,
        requested_endpoint: String,
        endpoint: String,
        format: Format,
        source: Box<dyn Source>,
    ) -> AudioResult<u64> {
        let id = self.next_stream.checked_add(1).ok_or("stream_limit")?;
        self.streams.insert(
            id,
            Stream {
                owner: owner.into(),
                requested_endpoint,
                endpoint,
                format,
                source,
                pending: None,
                failure: None,
            },
        );
        self.next_stream = id;
        Ok(id)
    }

    fn lookup(&self, owner: &str, operation: &str) -> AudioResult<Value> {
        validate_operation(operation)?;
        let record = self
            .operations
            .get(&(owner.into(), operation.into()))
            .ok_or("stream_not_found")?;
        let stream = self.streams.get(&record.stream).ok_or("stream_not_found")?;
        Ok(open_reply(record.stream, stream))
    }

    pub fn close(&mut self, owner: &str, id: u64) -> AudioResult<Value> {
        self.owned(owner, id)?;
        self.streams.remove(&id);
        Ok(json!({"ok":true}))
    }

    fn owned(&mut self, owner: &str, id: u64) -> AudioResult<&mut Stream> {
        self.streams
            .get_mut(&id)
            .filter(|stream| stream.owner == owner)
            .ok_or("stream_not_found")
    }

    fn status(&self, owner: &str, requested: Option<u64>) -> Value {
        let streams: Vec<_> = self.streams.iter().filter(|(id,stream)| stream.owner == owner && requested.is_none_or(|requested| requested == **id)).map(|(id,stream)| {
            let health = stream.source.health();
            json!({"stream":id,"endpoint":stream.endpoint,"requested_endpoint":stream.requested_endpoint,"format":stream.format,"state":if health.failure.is_some() { "failed" } else { "capturing" },"failure":health.failure,"discontinuities":health.discontinuities,"dropped_packets":health.dropped_packets})
        }).collect();
        json!({"ok":true,"enabled":self.enabled,"streams":streams})
    }

    pub fn shutdown(&mut self) {
        self.enabled = false;
        self.streams.clear();
    }
    /// # Safety
    /// The sink context and callback must remain live through this synchronous
    /// call and may only borrow samples for its callback's duration.
    pub unsafe fn read(&mut self, owner: &str, id: u64, sink: AudioSink) -> Status {
        let Ok(stream) = self.owned(owner, id) else {
            return MEDIA_CLOSED;
        };
        if stream.failure.or(stream.source.health().failure).is_some() {
            return MEDIA_BACKEND_ERROR;
        }
        if stream.pending.is_none() {
            match stream.source.read() {
                Ok(packet) => stream.pending = packet,
                Err(error) => {
                    stream.failure = Some(error);
                    return MEDIA_BACKEND_ERROR;
                }
            }
        }
        let Some(packet) = stream.pending.as_ref() else {
            return MEDIA_PENDING;
        };
        let frame = AudioFrame {
            sample_rate: stream.format.sample_rate,
            channels: stream.format.channels,
            format: stream.format.code(),
            frames: packet.frames,
            flags: packet.flags,
            data: packet.bytes.as_ptr(),
            len: packet.bytes.len(),
            timestamp_us: packet.timestamp_us,
            sequence: packet.sequence,
        };
        // SAFETY: pending owns all PCM bytes, retained on sink rejection.
        let status = unsafe { sink.send(&frame) };
        if status == STATUS_OK {
            stream.pending = None;
        }
        status
    }
}

fn open_reply(id: u64, stream: &Stream) -> Value {
    json!({"ok":true,"stream":id,"endpoint":stream.endpoint,"format":stream.format})
}

#[cfg(test)]
mod tests;
