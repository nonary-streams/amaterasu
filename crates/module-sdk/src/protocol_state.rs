//! Bounded State CAS journal for protocol obligations.
//!
//! This is a request/response state machine: the owner submits each StateRequest
//! with its authenticated State endpoint, waits for the terminal service result,
//! and never treats local queue admission as persistence. Production must select
//! and verify a State provider that implements durable CAS. This module cannot
//! prove that a provider's durability declaration is true. The journal holds at
//! most eight operation identities per authority epoch, including tombstones.
//! Confirmed cleanup and shared-authority epoch retirement are required before
//! those identities can be compacted. No time-based eviction is performed.
use crate::{
    protocol::{ObligationStore, PendingEffect, Reservation},
    protocol_authority::ConfirmedEpochRotation,
    state::{self, StateLookup, StateMutation, StateRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DURABLE_CAS_CAPABILITY: &str = "amaterasu.state.durable-cas/1";
const KEY: &str = "protocol-obligations/v1";
const MAX_ENTRIES: usize = 8;
const MAX_JOURNAL_BYTES: usize = 8_000;

/// Use a fresh value for every State mutation attempt, including cleanup and
/// compaction; a reused broker operation ID may replay an old success instead
/// of executing the intended CAS. Keep the ID through that one request only.
pub fn fresh_state_operation_id() -> Result<String, &'static str> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| "entropy_unavailable")?;
    let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!("protocol.state.{suffix}"))
}

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
    authority_epoch: Option<String>,
    entries: Vec<Entry>,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            version: 2,
            authority_epoch: None,
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
    if journal.version != 2
        || journal.entries.len() > MAX_ENTRIES
        || serde_json::to_vec(&journal)
            .map_err(|_| "invalid_journal")?
            .len()
            > MAX_JOURNAL_BYTES
    {
        return Err("invalid_journal");
    }
    if journal.authority_epoch.as_ref().is_some_and(|epoch| {
        epoch.is_empty() || epoch.len() > 128 || epoch.chars().any(char::is_control)
    }) || (!journal.entries.is_empty() && journal.authority_epoch.is_none())
    {
        return Err("invalid_journal");
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        if Some(&entry.effect.context.authority_epoch) != journal.authority_epoch.as_ref() {
            return Err("invalid_journal");
        }
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
    let epoch = &effect.context.authority_epoch;
    if epoch.is_empty() || epoch.len() > 128 || epoch.chars().any(char::is_control) {
        return Err("invalid_authority_epoch");
    }
    match &journal.authority_epoch {
        Some(current) if current != epoch => return Err("stale_authority_epoch"),
        None => journal.authority_epoch = Some(epoch.clone()),
        _ => {}
    }
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
        state_operation_id: fresh_state_operation_id()?,
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
    state_operation_id: String,
    effect: PendingEffect,
}
impl PendingWrite {
    pub fn request(&self) -> &StateRequest {
        &self.request
    }
    /// Use this fresh ID when submitting the State request to the broker.
    pub fn state_operation_id(&self) -> &str {
        &self.state_operation_id
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
impl ReadyReservation {
    pub(crate) fn from_confirmed(effect: PendingEffect) -> Self {
        Self {
            effect: Some(effect),
        }
    }
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
/// Submit the State write with a fresh `fresh_state_operation_id`.
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

/// Current persisted epoch for lost-commit reconciliation. A missing journal has
/// no epoch. This is State data, not proof of the authority's current epoch.
pub fn journal_epoch(response: &Value) -> Result<Option<String>, &'static str> {
    Ok(read_journal(response)?.1.authority_epoch)
}

/// Atomically retire an entire drained authority epoch after every cleanup is
/// independently confirmed. Pending or uncertain entries always block retirement.
/// The authority must reject old epochs before provider admission and admission to
/// the new epoch must remain closed until this CAS is confirmed or reconciled.
/// Do not reuse legacy v1 journals without a separate safe migration. Submit the
/// State write with a fresh `fresh_state_operation_id`.
pub fn prepare_epoch_retirement(
    _binding: &DurableStateBinding,
    response: &Value,
    rotation: &ConfirmedEpochRotation,
) -> Result<StateRequest, &'static str> {
    let (revision, mut journal) = read_journal(response)?;
    if journal.authority_epoch.as_deref() != Some(rotation.previous()) {
        return Err("stale_authority_epoch");
    }
    if journal.entries.iter().any(|entry| !entry.cleaned) {
        return Err("cleanup_unconfirmed");
    }
    journal.entries.clear();
    journal.authority_epoch = Some(rotation.current().into());
    set_journal(revision, &journal)
}
