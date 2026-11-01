//! Pairing-owned durable slot claims and payload-free effect identities.
//!
//! These CAS plans are not authorization, worker-retirement, cleanup or broker
//! receipts. The selected authority must verify those separately. An uncertain
//! write is reconciled from State and never treated as proof that an older
//! continuation stopped.
use crate::{
    broker_effect_fence::{
        EffectScope, GuardedEffectIdentity, SelectedEffectMethod, SourceSlotLease,
    },
    protocol_state::CleanupReference,
    state::{self, StateLookup, StateMutation, StateRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SLOT_COUNT: u8 = 64;
const WINDOW: usize = 8;
const MAX_VALUE_BYTES: usize = 12_000;

/// Issued by the trusted host lifecycle for one concrete provider instance.
/// This identifier is a selector, not a provider's self-asserted credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIncarnation {
    pub version: u8,
    pub id: String,
}

impl ProviderIncarnation {
    pub fn valid(&self) -> bool {
        self.version == 1
            && self.id.len() == 32
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub identity: GuardedEffectIdentity,
    /// Exact broker incarnation in which this effect was issued. A recovery
    /// lease may change, but old provider/host retirement must use this lease.
    pub issued_lease: SourceSlotLease,
    /// Must be retrieved from the selected host lifecycle, never inferred
    /// from the provider name or copied from client input.
    pub provider_incarnation: ProviderIncarnation,
    pub selected_method: SelectedEffectMethod,
    pub cleanup_method: crate::extension_api::MethodRef,
    pub cleanup_approval: amaterasu_contracts::ApiMethodApproval,
    pub cleanup: CleanupReference,
    /// Retained with the complete identity until broker compaction is confirmed.
    #[serde(default)]
    pub cleanup_confirmed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub lease: SourceSlotLease,
    pub scope: EffectScope,
    pub issuer: String,
    pub retired_through: u64,
    pub issued_through: u64,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    version: u8,
    pub slot: u8,
    pub retired_incarnation: u64,
    pub active: Option<Claim>,
    /// Retained until the broker confirms close or a restarted broker has
    /// independently verified every old worker and recovered its slots.
    #[serde(default)]
    pub pending_close: Option<Claim>,
}

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn key(slot: u8) -> Result<String, &'static str> {
    if slot >= SLOT_COUNT {
        return Err("invalid_slot");
    }
    Ok(format!("pairing/protocol-slot/v1/{slot:02}"))
}
pub fn read_request(slot: u8) -> Result<StateRequest, &'static str> {
    Ok(StateRequest::Get { key: key(slot)? })
}
fn valid_claim(slot: u8, retired: u64, claim: &Claim) -> bool {
    let source = &claim.scope.source;
    let source_valid = serde_json::to_value(source)
        .ok()
        .and_then(|value| serde_json::from_value::<crate::effect_authority::Source>(value).ok())
        .is_some_and(|source| source.valid());
    if !source_valid
        || source.slot() != Some(slot)
        || claim.lease.slot != slot
        || retired.checked_add(1) != Some(claim.lease.incarnation)
        || !token(&claim.lease.broker_epoch)
        || !token(&claim.lease.token)
        || !token(&claim.scope.authority_epoch)
        || !token(&claim.scope.issuer_epoch)
        || !amaterasu_contracts::valid_name(&claim.issuer)
        || claim.retired_through > claim.issued_through
        || claim.entries.len() > WINDOW
        || claim.issued_through.checked_sub(claim.retired_through)
            != Some(claim.entries.len() as u64)
    {
        return false;
    }
    let mut unconfirmed = false;
    claim.entries.iter().enumerate().all(|(index, entry)| {
        if !entry.cleanup_confirmed {
            unconfirmed = true;
        }
        let effect = &entry.identity;
        !(unconfirmed && entry.cleanup_confirmed)
            && effect.scope == claim.scope
            && entry.issued_lease.slot == slot
            && entry.issued_lease.incarnation == claim.lease.incarnation
            && entry.provider_incarnation.valid()
            && token(&entry.issued_lease.broker_epoch)
            && token(&entry.issued_lease.token)
            && claim.retired_through.checked_add(index as u64 + 1) == Some(effect.sequence)
            && digest(&effect.payload_sha256)
            && crate::protocol_state::CleanupReference::new(entry.cleanup.id.clone()).is_ok()
            && entry.selected_method.version == 1
            && entry.selected_method.provider == effect.provider
            && entry.selected_method.method.service == effect.service
            && entry.selected_method.method.method == effect.method
            && entry.selected_method.method.interface_major == effect.interface_major
            && entry.selected_method.method.interface_minor == effect.interface_minor
            && entry.selected_method.method.privilege == amaterasu_contracts::ApiPrivilege::User
            && entry.cleanup_approval.provider == effect.provider
            && entry.cleanup_approval.method.privilege == amaterasu_contracts::ApiPrivilege::User
            && entry.cleanup_approval.callers.contains(&claim.issuer)
            && entry.cleanup_approval.method.service == entry.cleanup_method.service
            && entry.cleanup_approval.method.method == entry.cleanup_method.method
            && entry.cleanup_approval.method.interface_major == entry.cleanup_method.interface_major
            && entry.cleanup_approval.method.interface_minor == entry.cleanup_method.interface_minor
            && entry.cleanup_method.schema_revision != 0
            && (entry.cleanup_method.service != effect.service
                || entry.cleanup_method.method != effect.method
                || entry.cleanup_method.interface_major != effect.interface_major)
            && amaterasu_contracts::valid_name(&effect.provider)
            && amaterasu_contracts::valid_name(&effect.capability)
            && amaterasu_contracts::valid_name(&effect.service)
            && amaterasu_contracts::valid_name(&effect.method)
            && amaterasu_contracts::valid_name(&effect.operation_id)
            && effect.interface_major != 0
            && effect.schema_revision != 0
            && (1..=30_000).contains(&effect.timeout_ms)
    })
}
pub fn read(slot: u8, response: &Value) -> Result<(u64, Slot), &'static str> {
    key(slot)?;
    let lookup: StateLookup = state::decode_response(response.clone())
        .map_err(|_| "invalid_state_response")?
        .map_err(|_| "state_unavailable")?;
    let value: Slot = if lookup.found {
        serde_json::from_value(lookup.value).map_err(|_| "invalid_slot_state")?
    } else {
        Slot {
            version: 1,
            slot,
            retired_incarnation: 0,
            active: None,
            pending_close: None,
        }
    };
    if value.version != 1
        || value.slot != slot
        || value
            .active
            .as_ref()
            .is_some_and(|claim| !valid_claim(slot, value.retired_incarnation, claim))
        || value.pending_close.as_ref().is_some_and(|claim| {
            value.active.is_some()
                || !claim.entries.is_empty()
                || claim.retired_through != claim.issued_through
                || value
                    .retired_incarnation
                    .checked_sub(1)
                    .is_none_or(|previous| !valid_claim(slot, previous, claim))
        })
        || serde_json::to_vec(&value)
            .map_err(|_| "invalid_slot_state")?
            .len()
            > MAX_VALUE_BYTES
    {
        return Err("invalid_slot_state");
    }
    Ok((lookup.revision, value))
}

/// This write must be sent with a fresh broker operation ID. `confirmed` only
/// verifies State's exact revision; it does not authorize opening or dispatch.
pub struct PendingWrite {
    request: StateRequest,
    revision: u64,
    operation_id: String,
}
impl PendingWrite {
    pub fn request(&self) -> &StateRequest {
        &self.request
    }
    pub fn state_operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn confirmed(self, response: Value) -> Result<(), &'static str> {
        let saved: StateMutation = state::decode_response(response)
            .map_err(|_| "invalid_state_response")?
            .map_err(|_| "state_write_failed")?;
        if self.revision.checked_add(1) != Some(saved.revision) {
            return Err("invalid_state_revision");
        }
        Ok(())
    }
}
fn write(revision: u64, slot: Slot) -> Result<PendingWrite, &'static str> {
    let slot_key = key(slot.slot)?;
    if slot
        .active
        .as_ref()
        .is_some_and(|claim| !valid_claim(slot.slot, slot.retired_incarnation, claim))
    {
        return Err("invalid_slot_state");
    }
    let value = serde_json::to_value(&slot).map_err(|_| "invalid_slot_state")?;
    // Reuse all read-side invariants, including pending-close consistency.
    read(
        slot.slot,
        &serde_json::json!({"ok":true,"revision":revision,"found":true,"value":value}),
    )?;
    if serde_json::to_vec(&value)
        .map_err(|_| "invalid_slot_state")?
        .len()
        > MAX_VALUE_BYTES
    {
        return Err("capacity");
    }
    Ok(PendingWrite {
        request: StateRequest::Set {
            key: slot_key,
            value,
            expected_revision: Some(revision),
        },
        revision,
        operation_id: crate::protocol_state::fresh_state_operation_id()?,
    })
}

/// Reserve an exact broker lease before OpenSlot. A retained existing claim
/// requires recovery, even if the caller repeats the same claim.
pub fn prepare_claim(
    response: &Value,
    lease: SourceSlotLease,
    scope: EffectScope,
    issuer: String,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(lease.slot, response)?;
    if slot.active.is_some() || slot.pending_close.is_some() {
        return Err("reconciliation_required");
    }
    slot.active = Some(Claim {
        lease,
        scope,
        issuer,
        retired_through: 0,
        issued_through: 0,
        entries: Vec::new(),
    });
    write(revision, slot)
}

/// Persist the exact identity and non-secret cleanup reference before ArmSelected.
/// The authority must first authenticate the source, issuer and selected policy.
pub fn prepare_issue(
    response: &Value,
    slot_number: u8,
    lease: &SourceSlotLease,
    entry: Entry,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    let claim = slot.active.as_mut().ok_or("unknown_claim")?;
    if &claim.lease != lease
        || &entry.issued_lease != lease
        || entry.identity.scope != claim.scope
        || entry.cleanup_confirmed
    {
        return Err("claim_mismatch");
    }
    if claim.entries.len() >= WINDOW {
        return Err("capacity");
    }
    if claim.issued_through.checked_add(1) != Some(entry.identity.sequence) {
        return Err("sequence_conflict");
    }
    claim.issued_through = entry.identity.sequence;
    claim.entries.push(entry);
    write(revision, slot)
}

/// Only actual retirement of the old broker's issuer, providers and host
/// workers permits a recovery lease. A new broker epoch alone is not proof.
pub trait OldWorkersRetired {
    fn verify(&self, _claim: &Claim, _new_lease: &SourceSlotLease) -> Result<(), &'static str> {
        Err("old_workers_retirement_unproven")
    }
}

/// Persist the new broker's exact recovery lease before OpenRecoverySlot. The
/// old effect leases remain in the entries for historical retirement checks.
pub fn prepare_recovery_lease(
    response: &Value,
    slot_number: u8,
    old_lease: &SourceSlotLease,
    new_lease: SourceSlotLease,
    retirement: &impl OldWorkersRetired,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    let claim = slot.active.as_mut().ok_or("unknown_claim")?;
    if &claim.lease != old_lease
        || old_lease.slot != new_lease.slot
        || old_lease.incarnation != new_lease.incarnation
        || old_lease.broker_epoch == new_lease.broker_epoch
        || !token(&new_lease.broker_epoch)
        || !token(&new_lease.token)
    {
        return Err("recovery_lease_mismatch");
    }
    retirement.verify(claim, &new_lease)?;
    claim.lease = new_lease;
    write(revision, slot)
}

/// The selected authority supplies proofs from the real broker and the
/// selected cleanup owner. Success means the exact effect is permanently fenced,
/// all provider work has retired, and its persistent cleanup is complete.
pub trait EffectRetirement {
    fn verify(&self, _lease: &SourceSlotLease, _entry: &Entry) -> Result<(), &'static str> {
        Err("retirement_unproven")
    }
}

/// Persist the oldest unconfirmed cleanup while retaining its exact historical
/// identity until the broker's corresponding compact is confirmed.
pub fn prepare_retire_effect(
    response: &Value,
    slot_number: u8,
    lease: &SourceSlotLease,
    entry: &Entry,
    retirement: &impl EffectRetirement,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    let claim = slot.active.as_mut().ok_or("unknown_claim")?;
    if &claim.lease != lease
        || claim.entries.iter().find(|item| !item.cleanup_confirmed) != Some(entry)
    {
        return Err("retirement_mismatch");
    }
    retirement.verify(lease, entry)?;
    let item = claim
        .entries
        .iter_mut()
        .find(|item| item.identity == entry.identity)
        .ok_or("retirement_mismatch")?;
    item.cleanup_confirmed = true;
    write(revision, slot)
}

/// A broker acknowledgment must refer to this exact lease and prefix. It is
/// not a substitute for the preceding independently confirmed cleanup.
pub trait BrokerCompaction {
    fn verify(&self, _lease: &SourceSlotLease, _through: u64) -> Result<(), &'static str> {
        Err("broker_compaction_unproven")
    }
}

/// Drop historical identities only after durable cleanup and broker compaction
/// of the same contiguous prefix have both been confirmed.
pub fn prepare_compacted(
    response: &Value,
    slot_number: u8,
    lease: &SourceSlotLease,
    through: u64,
    compaction: &impl BrokerCompaction,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    let claim = slot.active.as_mut().ok_or("unknown_claim")?;
    if &claim.lease != lease
        || through <= claim.retired_through
        || through > claim.issued_through
        || claim
            .entries
            .iter()
            .take_while(|entry| entry.identity.sequence <= through)
            .any(|entry| !entry.cleanup_confirmed)
    {
        return Err("compaction_mismatch");
    }
    compaction.verify(lease, through)?;
    claim
        .entries
        .retain(|entry| entry.identity.sequence > through);
    claim.retired_through = through;
    write(revision, slot)
}

/// The selected authority supplies proof that the exact slot is fenced and all
/// source, issuer, provider and host workers are retired and joined.
pub trait SourceRetirement {
    fn verify(&self, _claim: &Claim) -> Result<(), &'static str> {
        Err("source_retirement_unproven")
    }
}

/// Persist the retired incarnation before asking the broker to finish close.
/// A lost State or broker reply must be reconciled, never inferred from time.
pub fn prepare_retire_slot(
    response: &Value,
    slot_number: u8,
    lease: &SourceSlotLease,
    retirement: &impl SourceRetirement,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    let claim = slot.active.as_ref().ok_or("unknown_claim")?;
    if &claim.lease != lease
        || !claim.entries.is_empty()
        || claim.retired_through != claim.issued_through
    {
        return Err("source_retirement_pending");
    }
    retirement.verify(claim)?;
    slot.retired_incarnation = lease.incarnation;
    slot.pending_close = slot.active.take();
    write(revision, slot)
}

/// Proof must come from the real broker's exact slot status or verified broker
/// restart recovery, never from the persisted watermark by itself.
pub trait BrokerClose {
    fn verify(&self, _claim: &Claim) -> Result<(), &'static str> {
        Err("broker_close_unproven")
    }
}

pub fn prepare_clear_closed(
    response: &Value,
    slot_number: u8,
    close: &impl BrokerClose,
) -> Result<PendingWrite, &'static str> {
    let (revision, mut slot) = read(slot_number, response)?;
    close.verify(slot.pending_close.as_ref().ok_or("unknown_close")?)?;
    slot.pending_close = None;
    write(revision, slot)
}

/// A consistent pairing-namespace view; reading it does not establish that old
/// workers or persistent effects are retired.
pub struct RecoverySnapshot {
    pub revision: u64,
    pub retired_incarnations: Vec<u64>,
    pub active: Vec<Claim>,
    pub pending_close: Vec<Claim>,
}

/// Every fixed slot must be read at the same namespace revision. If any
/// concurrent State mutation changes that revision, restart the entire read.
pub fn recovery_snapshot(responses: &[(u8, Value)]) -> Result<RecoverySnapshot, &'static str> {
    if responses.len() != usize::from(SLOT_COUNT) {
        return Err("incomplete_recovery_snapshot");
    }
    let mut slots = std::collections::BTreeMap::new();
    let mut revision = None;
    for (index, response) in responses {
        let (current, slot) = read(*index, response)?;
        if revision.is_some_and(|expected| expected != current)
            || slots.insert(*index, slot).is_some()
        {
            return Err("inconsistent_recovery_snapshot");
        }
        revision = Some(current);
    }
    let mut retired_incarnations = Vec::with_capacity(usize::from(SLOT_COUNT));
    let mut active = Vec::new();
    let mut pending_close = Vec::new();
    for index in 0..SLOT_COUNT {
        let slot = slots.remove(&index).ok_or("incomplete_recovery_snapshot")?;
        retired_incarnations.push(slot.retired_incarnation);
        active.extend(slot.active);
        pending_close.extend(slot.pending_close);
    }
    Ok(RecoverySnapshot {
        revision: revision.ok_or("incomplete_recovery_snapshot")?,
        retired_incarnations,
        active,
        pending_close,
    })
}
