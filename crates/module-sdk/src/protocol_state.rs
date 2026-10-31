//! Bounded State CAS journal for protocol obligations.
//!
//! This is a request/response state machine: the owner submits each StateRequest
//! with its authenticated State endpoint, waits for the terminal service result,
//! and never treats local queue admission as persistence. Production must select
//! and verify a State provider that implements durable CAS. This module cannot
//! prove that a provider's durability declaration is true.
use crate::{
    protocol::{ObligationStore, PendingEffect, Reservation},
    state::{self, StateLookup, StateMutation, StateRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DURABLE_CAS_CAPABILITY: &str = "amaterasu.state.durable-cas/1";
const KEY: &str = "protocol-obligations/v1";
const MAX_ENTRIES: usize = 8;
const MAX_JOURNAL_BYTES: usize = 8_000;

/// Trusted composition assertion, not remote input or implementation proof.
/// The owner must bind every request to this actual State provider and caller,
/// and verify that its selected implementation offers durable atomic CAS.
#[derive(Clone, Debug)]
pub struct DurableStateBinding {
    owner: String,
    provider: String,
}
impl DurableStateBinding {
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn provider(&self) -> &str {
        &self.provider
    }
    pub fn new(owner: &str, provider: &str, capability: &str) -> Result<Self, &'static str> {
        if !amaterasu_contracts::valid_name(owner)
            || !amaterasu_contracts::valid_name(provider)
            || capability != DURABLE_CAS_CAPABILITY
        {
            return Err("durable_state_required");
        }
        Ok(Self {
            owner: owner.into(),
            provider: provider.into(),
        })
    }
}

/// A host-owned, non-secret reference into the selected provider's cleanup
/// registry. It is not a credential, device handle, certificate, or payload.
/// The trusted owner must ensure this identifier is safe to persist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupReference {
    pub id: String,
}
impl CleanupReference {
    pub fn new(id: String) -> Result<Self, &'static str> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid_cleanup_reference");
        }
        Ok(Self { id })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub effect: PendingEffect,
    pub cleanup: CleanupReference,
    pub cleaned: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    entries: Vec<Entry>,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}
fn read_journal(value: &Value) -> Result<(u64, Journal), &'static str> {
    let lookup = state::decode_response::<StateLookup>(value.clone())
        .map_err(|_| "invalid_state_response")?
        .map_err(|_| "state_error")?;
    let journal: Journal = if lookup.found {
        serde_json::from_value(lookup.value).map_err(|_| "invalid_journal")?
    } else {
        Journal::default()
    };
    if journal.version != 1
        || journal.entries.len() > MAX_ENTRIES
        || serde_json::to_vec(&journal)
            .map_err(|_| "invalid_journal")?
            .len()
            > MAX_JOURNAL_BYTES
    {
        return Err("invalid_journal");
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        CleanupReference::new(entry.cleanup.id.clone())?;
        if journal.entries[..index].iter().any(|old| {
            old.effect.caller == entry.effect.caller
                && old.effect.operation_id == entry.effect.operation_id
        }) {
            return Err("invalid_journal");
        }
    }
    Ok((lookup.revision, journal))
}
fn set_journal(revision: u64, journal: &Journal) -> Result<StateRequest, &'static str> {
    let value = serde_json::to_value(journal).map_err(|_| "invalid_journal")?;
    if serde_json::to_vec(&value)
        .map_err(|_| "invalid_journal")?
        .len()
        > MAX_JOURNAL_BYTES
    {
        return Err("capacity");
    }
    Ok(StateRequest::Set {
        key: KEY.into(),
        value,
        expected_revision: Some(revision),
    })
}
pub fn read_request() -> StateRequest {
    StateRequest::Get { key: KEY.into() }
}
/// Read back only after a completed State request, including during reconciliation.
pub fn entries(response: &Value) -> Result<Vec<Entry>, &'static str> {
    Ok(read_journal(response)?.1.entries)
}

/// The owner starts by reading the journal. A Set must use the same authenticated
/// endpoint and be awaited to a successful terminal State result before New exists.
pub fn prepare_reservation(
    _binding: &DurableStateBinding,
    response: &Value,
    effect: PendingEffect,
    cleanup: CleanupReference,
) -> Result<ReservationPlan, &'static str> {
    CleanupReference::new(cleanup.id.clone())?;
    let (revision, mut journal) = read_journal(response)?;
    if let Some(prior) = journal
        .entries
        .iter()
        .find(|e| e.effect.caller == effect.caller && e.effect.operation_id == effect.operation_id)
    {
        return if prior.effect == effect && prior.cleanup == cleanup {
            Ok(ReservationPlan::Existing)
        } else {
            Err("conflict")
        };
    }
    if journal.entries.len() >= MAX_ENTRIES {
        return Err("capacity");
    }
    journal.entries.push(Entry {
        effect: effect.clone(),
        cleanup,
        cleaned: false,
    });
    Ok(ReservationPlan::Write(Box::new(PendingWrite {
        request: set_journal(revision, &journal)?,
        expected_revision: revision,
        effect,
    })))
}
pub enum ReservationPlan {
    Existing,
    Write(Box<PendingWrite>),
}
pub struct PendingWrite {
    request: StateRequest,
    expected_revision: u64,
    effect: PendingEffect,
}
impl PendingWrite {
    pub fn request(&self) -> &StateRequest {
        &self.request
    }
    /// Match this terminal response to the exact request and endpoint; unknown or
    /// lost replies must not call this method. Reread and reconcile instead.
    pub fn confirmed(self, response: Value) -> Result<ReadyReservation, &'static str> {
        let mutation = state::decode_response::<StateMutation>(response)
            .map_err(|_| "invalid_state_response")?
            .map_err(|_| "state_write_failed")?;
        if self.expected_revision.checked_add(1) != Some(mutation.revision) {
            return Err("invalid_state_revision");
        }
        Ok(ReadyReservation {
            effect: Some(self.effect),
        })
    }
}
/// A one-shot receipt bound to exactly the journaled effect. It cannot be cloned.
pub struct ReadyReservation {
    effect: Option<PendingEffect>,
}
impl ObligationStore for ReadyReservation {
    fn reserve(&mut self, effect: &PendingEffect) -> Result<Reservation, &'static str> {
        match self.effect.take() {
            Some(expected) if &expected == effect => Ok(Reservation::New),
            _ => Err("reservation_mismatch"),
        }
    }
}
/// Caller must independently verify provider cleanup and worker retirement before
/// marking it complete. Tombstones retain operation identity and cannot be reused.
pub fn prepare_cleanup(
    _binding: &DurableStateBinding,
    response: &Value,
    effect: &PendingEffect,
    cleanup: &CleanupReference,
) -> Result<StateRequest, &'static str> {
    let (revision, mut journal) = read_journal(response)?;
    let entry = journal
        .entries
        .iter_mut()
        .find(|e| &e.effect == effect && &e.cleanup == cleanup)
        .ok_or("unknown_obligation")?;
    entry.cleaned = true;
    set_journal(revision, &journal)
}
