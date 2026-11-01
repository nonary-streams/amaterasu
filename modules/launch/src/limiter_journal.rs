//! Durable, owner-scoped identities for selected limiter cleanup.
//!
//! State is selected by trusted startup composition. An entry is written before
//! reserving an executor target, and is retained as a tombstone after the
//! executor confirms retirement. No client supplies the executor route.
use crate::records::Record;
use amaterasu_module_sdk::application_library::{
    limiter_target::RetirementRequest, preflight::PolicyIdentity,
};
use amaterasu_module_sdk::{Command, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const PREFIX: &str = "launch.limiter.v1.";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    ReservationPending,
    ExecutionPending,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub version: u8,
    pub owner: String,
    pub operation_id: String,
    pub certificate_id: String,
    pub token_digest: String,
    pub executor: String,
    pub executor_service: String,
    pub retirement: RetirementRequest,
    pub phase: Phase,
}

pub fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn key(owner: &str, operation_id: &str) -> String {
    format!("{PREFIX}{}", digest(&format!("{owner}\0{operation_id}")))
}

impl Entry {
    fn valid_stored(&self) -> bool {
        self.version == 1
            && self.owner == "amaterasu.nvhttp"
            && self.retirement.original_initiator == self.owner
            && self.retirement.validate().is_ok()
            && !self.operation_id.is_empty()
            && self.operation_id.len() <= 128
            && self.certificate_id.len() == 64
            && self.certificate_id.bytes().all(|b| b.is_ascii_hexdigit())
            && self.token_digest.len() == 64
            && self.token_digest.bytes().all(|b| b.is_ascii_hexdigit())
    }
    pub fn from_record(record: &Record, phase: Phase) -> Result<Self, &'static str> {
        let Some(crate::protocol::LaunchTarget::Provider { provider, .. }) = record.target.as_ref()
        else {
            return Err("invalid_workflow");
        };
        Ok(Self {
            version: 1,
            owner: record.owner.clone(),
            operation_id: record.operation_id.clone(),
            certificate_id: record
                .streaming_certificate_id
                .clone()
                .ok_or("invalid_workflow")?,
            token_digest: digest(
                record
                    .preflight_token
                    .as_deref()
                    .ok_or("invalid_workflow")?,
            ),
            executor: provider.clone(),
            executor_service: record
                .generic_provider_service
                .clone()
                .ok_or("invalid_workflow")?,
            retirement: crate::workflow::limiter_retirement_request(record)?,
            phase,
        })
    }

    pub fn matches_owner(
        &self,
        owner: &str,
        operation_id: &str,
        certificate_id: &str,
        token_digest: &str,
        policy_identity: &PolicyIdentity,
    ) -> bool {
        self.valid_stored()
            && self.owner == owner
            && self.operation_id == operation_id
            && self.certificate_id == certificate_id
            && self.token_digest == token_digest
            && &self.retirement.policy_identity == policy_identity
            && self.retirement.validate().is_ok()
    }
}

pub enum SweepResult {
    Pending,
    Complete(Vec<Entry>),
    Failed,
}

/// Read a revision-consistent bounded snapshot before replaying obligations.
pub struct Sweep {
    service: String,
    pending: Option<u64>,
    cursor: Option<Value>,
    entries: Vec<Entry>,
    revision: Option<u64>,
    last_after: Option<String>,
    nonce: String,
    sequence: u32,
}

impl Sweep {
    pub fn new(service: String) -> Result<Self, &'static str> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "random_unavailable")?;
        Ok(Self {
            service,
            pending: None,
            cursor: None,
            entries: Vec::new(),
            revision: None,
            last_after: None,
            nonce: nonce.iter().map(|byte| format!("{byte:02x}")).collect(),
            sequence: 0,
        })
    }

    pub fn tick(
        &mut self,
        send: &mut impl FnMut(Command) -> Result<Response, String>,
    ) -> Result<SweepResult, String> {
        if let Some(id) = self.pending.take() {
            match send(Command::TakeResult { request: id })? {
                Response::Pending => {
                    self.pending = Some(id);
                    return Ok(SweepResult::Pending);
                }
                Response::Result { payload } => {
                    if payload.get("next_cursor").is_none() {
                        return Ok(SweepResult::Failed);
                    }
                    let Some(revision) = payload["revision"].as_u64() else {
                        return Ok(SweepResult::Failed);
                    };
                    if self.revision.is_some_and(|previous| previous != revision)
                        || self.sequence > 4096
                    {
                        return Ok(SweepResult::Failed);
                    }
                    self.revision = Some(revision);
                    let Some(values) = payload["values"]
                        .as_object()
                        .filter(|values| payload["ok"] == true && values.len() <= 32)
                    else {
                        return Ok(SweepResult::Failed);
                    };
                    for (stored_key, value) in values {
                        if !stored_key.starts_with(PREFIX) {
                            continue;
                        }
                        let Ok(entry) = serde_json::from_value::<Entry>(value.clone()) else {
                            return Ok(SweepResult::Failed);
                        };
                        if !entry.valid_stored()
                            || *stored_key != key(&entry.owner, &entry.operation_id)
                        {
                            return Ok(SweepResult::Failed);
                        }
                        if entry.phase != Phase::Retired {
                            self.entries.push(entry);
                        }
                        if self.entries.len() > 4096 {
                            return Ok(SweepResult::Failed);
                        }
                    }
                    if payload["next_cursor"].is_null() {
                        return Ok(SweepResult::Complete(std::mem::take(&mut self.entries)));
                    }
                    let Some(after) = payload["next_cursor"]["after"].as_str() else {
                        return Ok(SweepResult::Failed);
                    };
                    if payload["next_cursor"]["revision"].as_u64() != Some(revision)
                        || self
                            .last_after
                            .as_deref()
                            .is_some_and(|previous| after <= previous)
                        || values.keys().next_back().is_none_or(|last| last != after)
                    {
                        return Ok(SweepResult::Failed);
                    }
                    self.last_after = Some(after.to_owned());
                    self.cursor = Some(payload["next_cursor"].clone());
                }
                _ => return Ok(SweepResult::Failed),
            }
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("sweep sequence exhausted")?;
        match send(Command::Request {
            service: self.service.clone(),
            payload: serde_json::json!({"action":"snapshot","cursor":self.cursor,"limit":32}),
            timeout_ms: 5_000,
            operation_id: format!("limiter-sweep-{}-{}", self.nonce, self.sequence),
            parent: None,
        })? {
            Response::Accepted { id } => {
                self.pending = Some(id);
                Ok(SweepResult::Pending)
            }
            _ => Ok(SweepResult::Failed),
        }
    }
}

pub fn read_for_record(
    record: &Record,
    payload: &Value,
) -> Result<(u64, Option<Phase>), &'static str> {
    if payload["ok"] != true {
        return Err("journal_unavailable");
    }
    let revision = payload["revision"]
        .as_u64()
        .ok_or("invalid_journal_response")?;
    if payload.get("value").is_none() {
        return Err("invalid_journal_response");
    }
    if payload["found"] == false && payload["value"].is_null() {
        return Ok((revision, None));
    }
    if payload["found"] != true {
        return Err("invalid_journal_response");
    }
    let existing: Entry =
        serde_json::from_value(payload["value"].clone()).map_err(|_| "invalid_journal_response")?;
    let expected = Entry::from_record(record, existing.phase.clone())?;
    if existing != expected {
        return Err("journal_identity_conflict");
    }
    Ok((revision, Some(existing.phase)))
}

pub fn validate_write(record: &Record, payload: &Value) -> Result<(), &'static str> {
    if payload["ok"] != true
        || payload["revision"].as_u64()
            != record
                .limiter_journal_revision
                .and_then(|revision| revision.checked_add(1))
    {
        return Err("journal_write_unconfirmed");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum RecoveryStep {
    Read,
    Stop,
    Retire,
    ReadForCommit,
    Commit,
}

/// Reconciles a cancellation after Launch has lost its in-memory record.
/// Every provider mutation is self-initiated and survives cancellation of the
/// requesting RPC. State's revision check fences delayed journal writes.
pub struct Recovery {
    request: Option<u64>,
    owner: String,
    operation_id: String,
    certificate_id: String,
    token_digest: String,
    policy: PolicyIdentity,
    service: String,
    pending: Option<u64>,
    step: RecoveryStep,
    entry: Option<Entry>,
    revision: Option<u64>,
    retries: u8,
    sequence: u32,
    nonce: String,
}

impl Recovery {
    pub fn new(
        request: u64,
        owner: String,
        operation_id: String,
        certificate_id: String,
        token: String,
        policy: PolicyIdentity,
        service: String,
    ) -> Result<Self, &'static str> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| "random_unavailable")?;
        let nonce = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            request: Some(request),
            owner,
            operation_id,
            certificate_id,
            token_digest: digest(&token),
            policy,
            service,
            pending: None,
            step: RecoveryStep::Read,
            entry: None,
            revision: None,
            retries: 0,
            sequence: 0,
            nonce,
        })
    }

    pub fn background(entry: Entry, service: String) -> Result<Self, &'static str> {
        let mut recovery = Self::new(
            0,
            entry.owner.clone(),
            entry.operation_id.clone(),
            entry.certificate_id.clone(),
            String::new(),
            entry.retirement.policy_identity.clone(),
            service,
        )?;
        recovery.request = None;
        recovery.token_digest = entry.token_digest.clone();
        recovery.entry = Some(entry);
        Ok(recovery)
    }

    fn finish(
        &self,
        send: &mut impl FnMut(Command) -> Result<Response, String>,
        ok: bool,
    ) -> Result<(), String> {
        let Some(request) = self.request else {
            return Ok(());
        };
        let payload = if ok {
            serde_json::json!({"ok":true,"cancelled":true,"operation_id":self.operation_id})
        } else {
            serde_json::json!({"ok":false,"code":"preflight_retirement_unconfirmed"})
        };
        match send(Command::Complete { request, payload })? {
            Response::Ok | Response::Error { .. } => Ok(()),
            _ => Err("recovery completion failed".into()),
        }
    }

    pub fn tick(
        &mut self,
        send: &mut impl FnMut(Command) -> Result<Response, String>,
        routes: &BTreeMap<String, crate::library::Route>,
    ) -> Result<Option<bool>, String> {
        if let Some(id) = self.pending.take() {
            match send(Command::TakeResult { request: id })? {
                Response::Pending => {
                    self.pending = Some(id);
                    return Ok(None);
                }
                Response::Result { payload } => {
                    if !self.consume(&payload, routes) {
                        self.finish(send, false)?;
                        return Ok(Some(false));
                    }
                    if matches!(self.step, RecoveryStep::Commit)
                        && self
                            .entry
                            .as_ref()
                            .is_some_and(|e| e.phase == Phase::Retired)
                    {
                        self.finish(send, true)?;
                        return Ok(Some(true));
                    }
                }
                _ => {
                    self.finish(send, false)?;
                    return Ok(Some(false));
                }
            }
        }
        let (service, payload) = match self.step {
            RecoveryStep::Read | RecoveryStep::ReadForCommit => (
                self.service.clone(),
                serde_json::json!({"action":"get","key":key(&self.owner,&self.operation_id)}),
            ),
            RecoveryStep::Stop => {
                let entry = self.entry.as_ref().ok_or("missing recovery entry")?;
                (
                    entry.executor_service.clone(),
                    serde_json::json!({"action":"recover_stop","operation_id":entry.retirement.operation_id,"application_id":entry.retirement.application_id,"provider_incarnation":entry.retirement.provider_incarnation}),
                )
            }
            RecoveryStep::Retire => {
                let entry = self.entry.as_ref().ok_or("missing recovery entry")?;
                (
                    entry.executor_service.clone(),
                    serde_json::json!({"action":"retire_limiter_target_v1","request":entry.retirement}),
                )
            }
            RecoveryStep::Commit => {
                let mut entry = self.entry.clone().ok_or("missing recovery entry")?;
                entry.phase = Phase::Retired;
                (
                    self.service.clone(),
                    serde_json::json!({"action":"set","key":key(&self.owner,&self.operation_id),"value":entry,"expected_revision":self.revision}),
                )
            }
        };
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("recovery sequence exhausted")?;
        match send(Command::Request {
            service,
            payload,
            timeout_ms: 5_000,
            operation_id: format!("limiter-recovery-{}-{}", self.nonce, self.sequence),
            parent: None,
        })? {
            Response::Accepted { id } => {
                self.pending = Some(id);
                Ok(None)
            }
            _ => {
                self.finish(send, false)?;
                Ok(Some(false))
            }
        }
    }

    fn consume(
        &mut self,
        payload: &Value,
        routes: &BTreeMap<String, crate::library::Route>,
    ) -> bool {
        match self.step {
            RecoveryStep::Read | RecoveryStep::ReadForCommit => {
                if payload["ok"] != true || payload["found"] != true {
                    return false;
                }
                let Some(revision) = payload["revision"].as_u64() else {
                    return false;
                };
                let Ok(entry) = serde_json::from_value::<Entry>(payload["value"].clone()) else {
                    return false;
                };
                if !entry.matches_owner(
                    &self.owner,
                    &self.operation_id,
                    &self.certificate_id,
                    &self.token_digest,
                    &self.policy,
                ) || entry.retirement.original_initiator != self.owner
                {
                    return false;
                }
                let PolicyIdentity::Library {
                    application,
                    policy_source: Some(source),
                    ..
                } = &entry.retirement.policy_identity
                else {
                    return false;
                };
                if !routes.get(&application.library).is_some_and(|route| {
                    route.callers.contains(&self.owner)
                        && route.streaming_policy_source.as_ref() == Some(source)
                        && route.executors.get(&entry.executor) == Some(&entry.executor_service)
                }) {
                    return false;
                }
                if self.entry.as_ref().is_some_and(|old| {
                    let mut old = old.clone();
                    old.phase = entry.phase.clone();
                    old != entry
                }) {
                    return false;
                }
                self.revision = Some(revision);
                let retired = entry.phase == Phase::Retired;
                self.entry = Some(entry);
                self.step = if retired {
                    RecoveryStep::Commit
                } else if matches!(self.step, RecoveryStep::Read) {
                    RecoveryStep::Stop
                } else {
                    RecoveryStep::Commit
                };
                true
            }
            RecoveryStep::Stop => {
                let Some(entry) = &self.entry else {
                    return false;
                };
                let expected = crate::provider_evidence::Expected {
                    operation_id: &entry.retirement.operation_id,
                    application_id: &entry.retirement.application_id,
                    provider_incarnation: &entry.retirement.provider_incarnation,
                    ticket: None,
                };
                if !crate::provider_evidence::validate(payload, &expected)
                    .is_ok_and(|proof| proof.retired)
                {
                    return false;
                }
                self.step = RecoveryStep::Retire;
                true
            }
            RecoveryStep::Retire => {
                let Some(entry) = &self.entry else {
                    return false;
                };
                let Ok(proof) = serde_json::from_value::<
                    amaterasu_module_sdk::application_library::limiter_target::Retirement,
                >(payload["retirement"].clone()) else {
                    return false;
                };
                if payload["ok"] != true || proof.validate_for(&entry.retirement).is_err() {
                    return false;
                }
                self.step = RecoveryStep::ReadForCommit;
                true
            }
            RecoveryStep::Commit => {
                if payload["ok"] == true
                    && payload["revision"].as_u64() == self.revision.and_then(|v| v.checked_add(1))
                {
                    if let Some(entry) = &mut self.entry {
                        entry.phase = Phase::Retired;
                    }
                    true
                } else if payload["error"]["code"] == "revision_conflict" && self.retries < 8 {
                    self.retries += 1;
                    self.step = RecoveryStep::ReadForCommit;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn sweep_rejects_a_snapshot_that_changes_revision_between_pages() {
        let mut sweep = Sweep::new("amaterasu.state/1".into()).unwrap();
        let mut replies = VecDeque::from([
            Response::Accepted { id: 1 },
            Response::Result {
                payload: serde_json::json!({"ok":true,"revision":7,"values":{"other":null},"next_cursor":{"revision":7,"after":"other"}}),
            },
            Response::Accepted { id: 2 },
            Response::Result {
                payload: serde_json::json!({"ok":true,"revision":8,"values":{},"next_cursor":null}),
            },
        ]);
        let mut send = |_command| Ok(replies.pop_front().expect("scripted broker response"));
        assert!(matches!(
            sweep.tick(&mut send).unwrap(),
            SweepResult::Pending
        ));
        assert!(matches!(
            sweep.tick(&mut send).unwrap(),
            SweepResult::Pending
        ));
        assert!(matches!(
            sweep.tick(&mut send).unwrap(),
            SweepResult::Failed
        ));
    }
}
