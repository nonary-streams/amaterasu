//! Recovery of a previous stream owner's admitted feedback sources. All state
//! is bounded by Pairing's immutable snapshot and no source is re-admitted.
use super::{Send, registry::Transport, worker::Call};
use amaterasu_module_sdk::{
    effect_authority::{self, StreamRetirement},
    effect_owner_recovery::{self, FenceReceipt, Page, SourceRecord},
    effect_source_retirement::{Receipt as SourceReceipt, State},
    transport::control_retirement::{Receipt as ControlReceipt, Request as ControlRequest},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum Phase {
    Status,
    Fence,
    Read(u32),
    Lookup(usize),
    FenceSource(usize),
    Retire(usize),
    CompleteLease(usize),
    AckLease(usize),
    CompleteSource(usize),
    RecheckSource(usize),
    Confirm(usize),
    DrainStatus,
    Drain(u64),
    Recover,
    VerifyRotation,
    Done,
}

pub(super) struct Restart {
    phase: Phase,
    call: Option<Call>,
    epoch: String,
    old_owner: String,
    new_owner: String,
    broker_nonce: String,
    selected_transport: String,
    operation: String,
    fence: Option<FenceReceipt>,
    records: Vec<SourceRecord>,
    source_operations: Vec<Option<String>>,
    source_retired: Vec<bool>,
    receipts: BTreeMap<Transport, StreamRetirement>,
    attempt: u64,
}
impl Restart {
    pub fn new(selected_transport: String) -> Option<Self> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).ok()?;
        Some(Self {
            phase: Phase::Status,
            call: None,
            epoch: String::new(),
            old_owner: String::new(),
            new_owner: String::new(),
            broker_nonce: random.iter().map(|byte| format!("{byte:02x}")).collect(),
            selected_transport,
            operation: String::new(),
            fence: None,
            records: Vec::new(),
            source_operations: Vec::new(),
            source_retired: Vec::new(),
            receipts: BTreeMap::new(),
            attempt: 0,
        })
    }
    fn transport(record: &SourceRecord) -> Transport {
        Transport {
            service: record.scope.transport_service.clone(),
            stream: record.scope.transport_stream,
            epoch: record.scope.transport_epoch.clone(),
        }
    }
    fn source_id(record: &SourceRecord) -> String {
        let digest = Sha256::digest(serde_json::to_vec(&record.source).expect("source serializes"));
        format!(
            "owner-restart-{}",
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
    fn receipt(&self, index: usize) -> Option<&StreamRetirement> {
        self.receipts
            .get(&Self::transport(self.records.get(index)?))
    }
    fn retained_operation(&self, transport: &Transport) -> Option<Option<String>> {
        let mut found = None;
        for record in self
            .records
            .iter()
            .filter(|r| Self::transport(r) == *transport)
        {
            for receipt in [
                record.transport_retirement.as_ref(),
                record
                    .lease
                    .as_ref()
                    .and_then(|lease| lease.retirement.as_ref()),
                record
                    .source_retirement
                    .as_ref()
                    .and_then(|source| source.retirement.as_ref()),
            ]
            .into_iter()
            .flatten()
            {
                if !receipt.matches(&record.scope)
                    || found
                        .as_ref()
                        .is_some_and(|old: &StreamRetirement| old != receipt)
                {
                    return None;
                }
                found = Some(receipt.clone());
            }
        }
        Some(found.map(|receipt| receipt.operation_id))
    }
    fn make_call(&mut self, service: &str, payload: Value, stage: &str) -> Option<Call> {
        self.attempt = self.attempt.checked_add(1)?;
        Some(Call::new(
            service,
            payload,
            format!("restart-{}-{stage}-{}", self.broker_nonce, self.attempt),
        ))
    }
    /// Advances one broker step. False includes every unavailable, invalid or
    /// uncertain case; only an exact drained and rotated authority returns true.
    pub fn advance(&mut self, send: &mut Send<'_>) -> bool {
        self.advance_phase_boundary();
        if matches!(self.phase, Phase::Done) {
            return true;
        }
        if self.call.is_none() {
            self.call = self.next_call();
        }
        let Some(response) = self.call.as_mut().and_then(|call| call.poll(send)) else {
            return false;
        };
        self.call = None;
        self.handle_response(response)
    }

    fn next_call(&mut self) -> Option<Call> {
        let phase = self.phase;
        let (service, payload, stage) = match phase {
            Phase::Status | Phase::DrainStatus | Phase::VerifyRotation => (
                effect_authority::SERVICE.to_string(),
                json!({"action":"status"}),
                "status",
            ),
            Phase::Fence => (
                effect_authority::SERVICE.into(),
                json!({"action":"fence_effect_owner_restart","version":1,"epoch":self.epoch,"owner_epoch":self.old_owner,"operation_id":self.operation}),
                "fence",
            ),
            Phase::Read(index) => (
                effect_authority::SERVICE.into(),
                json!({"action":"read_effect_owner_restart","version":1,"epoch":self.epoch,"owner_epoch":self.old_owner,"operation_id":self.operation,"index":index}),
                "read",
            ),
            Phase::Lookup(i) | Phase::RecheckSource(i) => (
                effect_authority::SERVICE.into(),
                json!({"action":"lookup_current_source_retirement","epoch":self.epoch,"source":self.records[i].source,"scope":self.records[i].scope}),
                "lookup",
            ),
            Phase::FenceSource(i) => (
                effect_authority::SERVICE.into(),
                json!({"action":"fence_source","epoch":self.epoch,"source":self.records[i].source,"scope":self.records[i].scope,"operation_id":Self::source_id(&self.records[i])}),
                "source-fence",
            ),
            Phase::Retire(i) => {
                let transport = Self::transport(&self.records[i]);
                if self.receipts.contains_key(&transport) {
                    self.phase = Phase::Retire(i + 1);
                    return None;
                }
                let retained = self.retained_operation(&transport)?;
                let operation = retained.unwrap_or_else(|| transport.retirement_operation());
                let payload = serde_json::to_value(ControlRequest::Retire {
                    stream: transport.stream,
                    operation_id: &operation,
                    provider_epoch: &transport.epoch,
                })
                .expect("retirement serializes");
                (transport.service, payload, "retire")
            }
            Phase::CompleteLease(i) => {
                let Some(lease) = &self.records[i].lease else {
                    self.phase = Phase::CompleteLease(i + 1);
                    return None;
                };
                if lease.complete {
                    self.phase = Phase::CompleteLease(i + 1);
                    return None;
                }
                let receipt = self.receipt(i)?;
                (
                    effect_authority::SERVICE.into(),
                    json!({"action":"complete_stream_lease","epoch":self.epoch,"operation_id":lease.operation_id,"ticket":lease.ticket,"retirement":receipt}),
                    "lease-complete",
                )
            }
            Phase::AckLease(i) => {
                let Some(lease) = &self.records[i].lease else {
                    self.phase = Phase::AckLease(i + 1);
                    return None;
                };
                (
                    effect_authority::SERVICE.into(),
                    json!({"action":"acknowledge","epoch":self.epoch,"operation_id":lease.operation_id,"ticket":lease.ticket}),
                    "lease-ack",
                )
            }
            Phase::CompleteSource(i) => {
                if self.source_retired[i] {
                    self.phase = Phase::CompleteSource(i + 1);
                    return None;
                }
                let Some(operation) = &self.source_operations[i] else {
                    return None;
                };
                let receipt = self.receipt(i)?;
                (
                    effect_authority::SERVICE.into(),
                    json!({"action":"complete_source_retirement","epoch":self.epoch,"source":self.records[i].source,"scope":self.records[i].scope,"operation_id":operation,"retirement":receipt}),
                    "source-complete",
                )
            }
            Phase::Confirm(i) => {
                let receipt = self.receipt(i)?;
                (
                    effect_authority::SERVICE.into(),
                    json!({"action":"confirm_effect_owner_restart","version":1,"epoch":self.epoch,"owner_epoch":self.old_owner,"operation_id":self.operation,"index":i,"retirement":receipt}),
                    "confirm",
                )
            }
            Phase::Drain(generation) => (
                effect_authority::SERVICE.into(),
                json!({"action":"drain","epoch":self.epoch,"fence_generation":generation}),
                "drain",
            ),
            Phase::Recover => (
                effect_authority::SERVICE.into(),
                json!({"action":"recover","epoch":self.epoch,"owner_epoch":self.new_owner}),
                "recover",
            ),
            Phase::Done => return None,
        };
        self.make_call(&service, payload, stage)
    }

    fn handle_response(&mut self, response: Value) -> bool {
        match self.phase {
            Phase::Status => {
                let Some(epoch) = response["epoch"].as_str().filter(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }) else {
                    return false;
                };
                let Some(owner) = response["owner_epoch"].as_str().filter(|s| {
                    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
                }) else {
                    return false;
                };
                if response["ok"] != true || response["startup_recovery_required"] != false {
                    return false;
                }
                self.epoch = epoch.into();
                self.old_owner = owner.into();
                let digest =
                    Sha256::digest(format!("{}\0{}", self.epoch, self.old_owner).as_bytes());
                self.operation = format!(
                    "owner-restart-{}",
                    digest
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                let owner_digest = Sha256::digest(
                    format!("new-owner\0{}\0{}", self.epoch, self.old_owner).as_bytes(),
                );
                self.new_owner = owner_digest[..16]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                self.phase = Phase::Fence;
            }
            Phase::Fence => {
                if let Ok(fence) = serde_json::from_value::<FenceReceipt>(response)
                    && fence.matches(&self.epoch, &self.old_owner, &self.operation)
                {
                    let count = fence.count;
                    self.fence = Some(fence);
                    self.phase = if count == 0 {
                        Phase::DrainStatus
                    } else {
                        Phase::Read(0)
                    };
                }
            }
            Phase::Read(index) => {
                if let Ok(page) = serde_json::from_value::<Page>(response)
                    && self
                        .fence
                        .as_ref()
                        .is_some_and(|fence| page.matches(fence, index))
                    && page.record.scope.transport_service == self.selected_transport
                    && !self
                        .records
                        .iter()
                        .any(|record| record.source == page.record.source)
                {
                    self.records.push(page.record);
                    self.source_operations.push(None);
                    self.source_retired.push(false);
                    self.phase = if index + 1 == page.count {
                        Phase::Lookup(0)
                    } else {
                        Phase::Read(index + 1)
                    };
                }
            }
            Phase::Lookup(i) => {
                if response["ok"] == false && response["code"] == "unknown_source_retirement" {
                    self.phase = Phase::FenceSource(i);
                } else if let Ok(receipt) = serde_json::from_value::<SourceReceipt>(response)
                    && receipt.matches_source(
                        &self.epoch,
                        &self.records[i].source,
                        &self.records[i].scope,
                    )
                    && matches!(receipt.state, State::Fenced | State::Retired)
                {
                    self.source_retired[i] = receipt.state == State::Retired;
                    self.source_operations[i] = Some(receipt.operation_id);
                    self.phase = Phase::Lookup(i + 1);
                }
            }
            Phase::FenceSource(i) => {
                if response["ok"] == false && response["code"] == "source_retirement_conflict" {
                    self.phase = Phase::Lookup(i);
                } else if let Ok(receipt) = serde_json::from_value::<SourceReceipt>(response)
                    && receipt.matches(
                        &self.epoch,
                        &self.records[i].source,
                        &self.records[i].scope,
                        &Self::source_id(&self.records[i]),
                    )
                {
                    self.source_retired[i] = receipt.state == State::Retired;
                    self.source_operations[i] = Some(receipt.operation_id);
                    self.phase = Phase::Lookup(i + 1);
                }
            }
            Phase::Retire(i) => {
                let transport = Self::transport(&self.records[i]);
                let Some(operation) = self
                    .retained_operation(&transport)
                    .map(|op| op.unwrap_or_else(|| transport.retirement_operation()))
                else {
                    return false;
                };
                if let Ok(receipt) = serde_json::from_value::<ControlReceipt>(response)
                    && receipt.matches(transport.stream, &operation, &transport.epoch)
                {
                    let retired = StreamRetirement {
                        ok: true,
                        service: transport.service.clone(),
                        stream: receipt.stream,
                        provider_epoch: receipt.provider_epoch,
                        operation_id: receipt.operation_id,
                        scope: receipt.scope,
                        state: receipt.state,
                    };
                    if self
                        .records
                        .iter()
                        .filter(|r| Self::transport(r) == transport)
                        .all(|r| {
                            [
                                r.transport_retirement.as_ref(),
                                r.lease.as_ref().and_then(|l| l.retirement.as_ref()),
                                r.source_retirement
                                    .as_ref()
                                    .and_then(|s| s.retirement.as_ref()),
                            ]
                            .into_iter()
                            .flatten()
                            .all(|old| old == &retired)
                        })
                    {
                        self.receipts.insert(transport, retired);
                        self.phase = Phase::Retire(i + 1);
                    }
                }
            }
            Phase::CompleteLease(i) | Phase::AckLease(i) => {
                let lease = self.records[i].lease.as_ref().expect("lease selected");
                if response["ok"] == true
                    && response["epoch"] == self.epoch
                    && response["operation_id"] == lease.operation_id
                {
                    self.phase = if matches!(self.phase, Phase::CompleteLease(_)) {
                        Phase::CompleteLease(i + 1)
                    } else {
                        Phase::AckLease(i + 1)
                    };
                }
            }
            Phase::CompleteSource(i) => {
                if response["ok"] == false && response["code"] == "source_retirement_conflict" {
                    self.phase = Phase::RecheckSource(i);
                } else if let Ok(receipt) = serde_json::from_value::<SourceReceipt>(response)
                    && self.source_operations[i].as_ref().is_some_and(|op| {
                        receipt.matches(
                            &self.epoch,
                            &self.records[i].source,
                            &self.records[i].scope,
                            op,
                        )
                    })
                    && receipt.state == State::Retired
                {
                    self.phase = Phase::CompleteSource(i + 1);
                }
            }
            Phase::RecheckSource(i) => {
                if let Ok(receipt) = serde_json::from_value::<SourceReceipt>(response)
                    && self.source_operations[i].as_ref().is_some_and(|op| {
                        receipt.matches(
                            &self.epoch,
                            &self.records[i].source,
                            &self.records[i].scope,
                            op,
                        )
                    })
                    && receipt.state == State::Retired
                {
                    self.source_retired[i] = true;
                    self.phase = Phase::CompleteSource(i + 1);
                }
            }
            Phase::Confirm(i) => {
                if let Ok(receipt) =
                    serde_json::from_value::<effect_owner_recovery::ConfirmationReceipt>(response)
                    && self
                        .fence
                        .as_ref()
                        .is_some_and(|f| receipt.matches(f, i as u32))
                {
                    self.phase = Phase::Confirm(i + 1);
                }
            }
            Phase::DrainStatus => {
                if response["ok"] == true
                    && response["epoch"] == self.epoch
                    && response["owner_epoch"] == self.old_owner
                    && response["startup_recovery_required"] == false
                    && response["fenced"] == true
                    && let Some(generation) =
                        response["fence_generation"].as_u64().filter(|g| *g != 0)
                {
                    self.phase = Phase::Drain(generation);
                }
            }
            Phase::Drain(generation) => {
                if response["ok"] == true
                    && response["epoch"] == self.epoch
                    && response["fence_generation"] == generation
                {
                    self.phase = Phase::Recover;
                } else if response["code"] == "stale_fence" {
                    self.phase = Phase::DrainStatus;
                }
            }
            Phase::Recover => {
                if (response["ok"] == true
                    && response["epoch"]
                        .as_str()
                        .is_some_and(|epoch| epoch.len() == 32 && epoch != self.epoch))
                    || response["code"] == "stale_epoch"
                {
                    self.phase = Phase::VerifyRotation;
                }
            }
            Phase::VerifyRotation => {
                if response["ok"] == true
                    && response["owner_epoch"] == self.new_owner
                    && response["startup_recovery_required"] == false
                    && response["fenced"] == false
                    && response["drain_required"] == false
                    && response["in_flight"] == 0
                    && response["bound_sources"] == 0
                    && response["pending_source_retirements"] == 0
                    && response["epoch"]
                        .as_str()
                        .is_some_and(|epoch| epoch.len() == 32 && epoch != self.epoch)
                {
                    self.phase = Phase::Done;
                }
            }
            Phase::Done => return true,
        }
        self.advance_phase_boundary();
        matches!(self.phase, Phase::Done)
    }

    fn advance_phase_boundary(&mut self) {
        let len = self.records.len();
        self.phase = match self.phase {
            Phase::Lookup(i) if i == len => Phase::Retire(0),
            Phase::Retire(i) if i == len => Phase::CompleteLease(0),
            Phase::CompleteLease(i) if i == len => Phase::AckLease(0),
            Phase::AckLease(i) if i == len => Phase::CompleteSource(0),
            Phase::CompleteSource(i) if i == len => Phase::Confirm(0),
            Phase::Confirm(i) if i == len => Phase::DrainStatus,
            phase => phase,
        };
    }
}
