//! Opt-in v1 sequence journal for one trusted caller and session generation.
//!
//! Journal format version 4 binds each issuer to its lifecycle source,
//! effect-owner and broker-issuer incarnations and exact approval fingerprints.
//! Earlier journals fail closed and need safe
//! migration; they are never reset. The host allocates a fresh, contiguous
//! sequence in the same State CAS that records the effect. A sequence is not a
//! credential: authenticated ingress and the selected shared authority must
//! reject stale sources before dispatch.
//! Only confirmed cleanup advances the contiguous retired watermark. Retired
//! sequences are rejected, with no reconstruction of a result. Client-chosen
//! arbitrary operation IDs are not deduplicated by this opt-in contract. Use a
//! dedicated trusted broker caller for sequenced routes; do not mix arbitrary-ID
//! and sequenced operations on the same caller. Never reset or delete a source
//! journal while that source generation can still be admitted.
//!
//! Source journals persist for the life of their generation. No source deletion
//! or cross-generation reclamation is offered here; State's namespace limits
//! still bound the number of distinct sources. The owner must use non-secret
//! opaque source identifiers and a selected provider with durable atomic CAS.
use crate::{
    effect_authority::Source,
    extension_api::LifecycleTarget,
    protocol::{PendingEffect, SessionContext, valid_context},
    protocol_state::{
        CleanupReference, DurableStateBinding, ReadyReservation, fresh_state_operation_id,
    },
    state::{self, StateLookup, StateMutation, StateRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const WINDOW: usize = 8;
const MAX_BYTES: usize = 12_000;

/// Created solely from host-authenticated session context and broker caller.
/// This selector is not an admission grant and must never come from client JSON.
pub struct SequencedSource {
    caller: String,
    context: SessionContext,
    hash: String,
}
impl SequencedSource {
    pub fn new(caller: &str, context: &SessionContext) -> Result<Self, &'static str> {
        if !amaterasu_contracts::valid_name(caller) || !valid_context(context) {
            return Err("invalid_source");
        }
        // Preserve the original source key so existing v1 journals are found
        // and rejected, rather than silently starting a new sequence namespace.
        #[derive(Serialize)]
        struct OriginalSourceKey<'a> {
            authority_epoch: &'a str,
            target: &'a LifecycleTarget,
            peer_ref: &'a str,
        }
        let original = OriginalSourceKey {
            authority_epoch: &context.authority_epoch,
            target: &context.target,
            peer_ref: &context.peer_ref,
        };
        let encoded = serde_json::to_vec(&(caller, original)).map_err(|_| "invalid_source")?;
        let mut hash = Sha256::new();
        hash.update(b"amaterasu.protocol.source-sequence/v1\0");
        hash.update(encoded);
        Ok(Self {
            caller: caller.into(),
            context: context.clone(),
            hash: format!("{:x}", hash.finalize()),
        })
    }
    fn key(&self) -> String {
        format!("protocol-sequence/v1/{}", self.hash)
    }
    pub fn operation_id(&self, sequence: u64) -> Result<String, &'static str> {
        if sequence == 0 {
            return Err("invalid_sequence");
        }
        Ok(format!("ps1.{}.{sequence}", self.hash))
    }
    pub fn read_request(&self) -> StateRequest {
        StateRequest::Get { key: self.key() }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub sequence: u64,
    pub effect: PendingEffect,
    /// Lifecycle-issued source and effect-owner incarnation for this issuer.
    pub issuer: Source,
    pub cleanup: CleanupReference,
    pub cleaned: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    source: String,
    retired: u64,
    issued: u64,
    entries: Vec<Entry>,
}
impl Journal {
    fn empty(source: &SequencedSource) -> Self {
        Self {
            version: 4,
            source: source.hash.clone(),
            retired: 0,
            issued: 0,
            entries: Vec::new(),
        }
    }
}
fn read(source: &SequencedSource, response: &Value) -> Result<(u64, Journal), &'static str> {
    let lookup = state::decode_response::<StateLookup>(response.clone())
        .map_err(|_| "invalid_state_response")?
        .map_err(|_| "state_error")?;
    let journal: Journal = if lookup.found {
        serde_json::from_value(lookup.value).map_err(|_| "invalid_journal")?
    } else {
        Journal::empty(source)
    };
    if journal.version != 4
        || journal.source != source.hash
        || journal.retired > journal.issued
        || journal.entries.len() > WINDOW
        || journal.issued.checked_sub(journal.retired) != Some(journal.entries.len() as u64)
        || serde_json::to_vec(&journal)
            .map_err(|_| "invalid_journal")?
            .len()
            > MAX_BYTES
    {
        return Err("invalid_journal");
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        let sequence = journal
            .retired
            .checked_add(index as u64 + 1)
            .ok_or("invalid_journal")?;
        if entry.sequence != sequence
            || entry.effect.caller != source.caller
            || entry.effect.context != source.context
            || !source_matches_context(&entry.issuer, &source.context)
            || entry.effect.operation_id != source.operation_id(sequence)?
            || CleanupReference::new(entry.cleanup.id.clone()).is_err()
        {
            return Err("invalid_journal");
        }
    }
    if journal.entries.first().is_some_and(|entry| entry.cleaned) {
        return Err("invalid_journal");
    }
    Ok((lookup.revision, journal))
}
fn set(
    source: &SequencedSource,
    revision: u64,
    journal: &Journal,
) -> Result<StateRequest, &'static str> {
    let value = serde_json::to_value(journal).map_err(|_| "invalid_journal")?;
    if serde_json::to_vec(&value)
        .map_err(|_| "invalid_journal")?
        .len()
        > MAX_BYTES
    {
        return Err("capacity");
    }
    Ok(StateRequest::Set {
        key: source.key(),
        value,
        expected_revision: Some(revision),
    })
}
/// Bounded durable identities for the trusted recovery coordinator. This is
/// State data only: it does not fence an issuer, prove a provider stopped, or
/// authorize opening a source. Payload bytes and credentials are not included.
#[derive(Debug)]
pub struct RecoverySnapshot {
    pub retired_through: u64,
    pub issued_through: u64,
    pub unresolved: Vec<Entry>,
}

/// Read after a terminal State result from the selected durable provider.
/// Entries marked cleaned remain visible when an earlier sequence is pending.
pub fn recovery_snapshot(
    source: &SequencedSource,
    response: &Value,
) -> Result<RecoverySnapshot, &'static str> {
    let (_, journal) = read(source, response)?;
    Ok(RecoverySnapshot {
        retired_through: journal.retired,
        issued_through: journal.issued,
        unresolved: journal.entries,
    })
}

/// The durable contiguous retirement watermark, not a provider result.
/// Opening a broker source additionally requires the authority to verify old
/// workers are retired and every unresolved record is reconciled.
pub fn retired_through(source: &SequencedSource, response: &Value) -> Result<u64, &'static str> {
    Ok(read(source, response)?.1.retired)
}

/// A watermark usable during source recovery only after all journal entries are
/// reconciled. It does not prove retirement of old workers, authenticate the
/// source, or authorize the broker's OpenSource operation.
pub fn recovery_watermark(source: &SequencedSource, response: &Value) -> Result<u64, &'static str> {
    let (_, journal) = read(source, response)?;
    if !journal.entries.is_empty() {
        return Err("reconciliation_required");
    }
    Ok(journal.retired)
}

/// A candidate is not issued until the associated CAS is confirmed. Keep its
/// sequence for reconciliation if the write response is lost.
pub fn next_sequence(source: &SequencedSource, response: &Value) -> Result<u64, &'static str> {
    let (_, journal) = read(source, response)?;
    if journal.entries.len() == WINDOW {
        return Err("capacity");
    }
    journal.issued.checked_add(1).ok_or("sequence_exhausted")
}
/// Explicit outcome: Retired never implies success or provides a result.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// This sequence was compacted; no previous result is implied.
    Retired,
    Existing(Box<Entry>),
    /// Absent from this snapshot; an outstanding earlier CAS may still commit.
    NotIssued,
}
pub fn lookup(
    source: &SequencedSource,
    response: &Value,
    sequence: u64,
) -> Result<Lookup, &'static str> {
    if sequence == 0 {
        return Err("invalid_sequence");
    }
    let (_, journal) = read(source, response)?;
    if sequence <= journal.retired {
        return Ok(Lookup::Retired);
    }
    if sequence > journal.issued {
        return Ok(Lookup::NotIssued);
    }
    journal
        .entries
        .into_iter()
        .find(|entry| entry.sequence == sequence)
        .map(|entry| Lookup::Existing(Box::new(entry)))
        .ok_or("invalid_journal")
}
fn source_matches_context(issuer: &Source, context: &SessionContext) -> bool {
    issuer.valid()
        && issuer.effect_owner_epoch == context.effect_owner_epoch
        && matches!(&context.target, LifecycleTarget::Stream { host_epoch, session_id, stream_id, generation }
            if issuer.host_epoch == *host_epoch && issuer.session_id == *session_id
            && issuer.stream_id == *stream_id && issuer.stream_generation == *generation)
}

/// Evidence supplied by the selected authority/runtime, never client input.
/// Success must mean the exact issuing continuation for this source, owner
/// incarnation and effect is permanently unable to admit or deliver any effect,
/// and all calls admitted before the fence have retired. A State read, timeout,
/// cleanup reply, or unverified owner assertion does not establish retirement.
/// This hook is not a new authority: production must adapt the existing trusted
/// runtime and shared effect authority, and must fail closed until it can.
pub trait IssuerRetirement {
    fn verify_retired(
        &self,
        _issuer: &Source,
        _effect: &PendingEffect,
    ) -> Result<(), &'static str> {
        Err("issuer_retirement_unproven")
    }
    /// Separately verify the selected provider's persistent cleanup for this
    /// exact non-secret reference. Broker worker retirement is not cleanup.
    fn verify_cleanup(
        &self,
        _issuer: &Source,
        _effect: &PendingEffect,
        _cleanup: &CleanupReference,
    ) -> Result<(), &'static str> {
        Err("cleanup_unproven")
    }
}

/// The effect must use the candidate operation ID returned by operation_id.
/// Match the State reply to this exact CAS and selected endpoint.
pub fn prepare_issue(
    _binding: &DurableStateBinding,
    source: &SequencedSource,
    response: &Value,
    effect: PendingEffect,
    issuer: Source,
    cleanup: CleanupReference,
) -> Result<PendingIssue, &'static str> {
    CleanupReference::new(cleanup.id.clone())?;
    let (revision, mut journal) = read(source, response)?;
    if journal.entries.len() >= WINDOW {
        return Err("capacity");
    }
    let sequence = journal.issued.checked_add(1).ok_or("sequence_exhausted")?;
    if effect.caller != source.caller
        || effect.context != source.context
        || !source_matches_context(&issuer, &source.context)
        || effect.operation_id != source.operation_id(sequence)?
    {
        return Err("identity_mismatch");
    }
    journal.issued = sequence;
    journal.entries.push(Entry {
        sequence,
        effect: effect.clone(),
        issuer,
        cleanup,
        cleaned: false,
    });
    let state_operation_id = fresh_state_operation_id()?;
    Ok(PendingIssue {
        request: set(source, revision, &journal)?,
        state_operation_id,
        revision,
        effect,
    })
}
pub struct PendingIssue {
    request: StateRequest,
    state_operation_id: String,
    revision: u64,
    effect: PendingEffect,
}
impl PendingIssue {
    pub fn request(&self) -> &StateRequest {
        &self.request
    }
    /// Submit the State request with this fresh broker operation ID. Reusing a
    /// prior State broker ID can replay an old success without executing CAS.
    pub fn state_operation_id(&self) -> &str {
        &self.state_operation_id
    }
    /// A lost or uncertain reply never yields this receipt; reread by sequence.
    pub fn confirmed(self, response: Value) -> Result<ReadyReservation, &'static str> {
        let mutation = state::decode_response::<StateMutation>(response)
            .map_err(|_| "invalid_state_response")?
            .map_err(|_| "state_write_failed")?;
        if self.revision.checked_add(1) != Some(mutation.revision) {
            return Err("invalid_state_revision");
        }
        Ok(ReadyReservation::from_confirmed(self.effect))
    }
}
/// The owner must independently confirm the exact provider cleanup and worker
/// retirement. The issuing continuation must also be permanently fenced by the
/// selected authority/runtime before this function marks or compacts the entry;
/// this excludes delayed successful State replies from retired dispatchers.
/// The CAS compacts only the cleaned contiguous prefix; gaps remain.
/// Submit this write with a fresh `protocol_state::fresh_state_operation_id`.
pub fn prepare_cleanup(
    _binding: &DurableStateBinding,
    source: &SequencedSource,
    response: &Value,
    sequence: u64,
    effect: &PendingEffect,
    cleanup: &CleanupReference,
    retirement: &impl IssuerRetirement,
) -> Result<StateRequest, &'static str> {
    let (revision, mut journal) = read(source, response)?;
    if sequence <= journal.retired {
        return Err("retired");
    }
    let entry = journal
        .entries
        .iter_mut()
        .find(|entry| {
            entry.sequence == sequence && &entry.effect == effect && &entry.cleanup == cleanup
        })
        .ok_or("unknown_obligation")?;
    retirement.verify_retired(&entry.issuer, &entry.effect)?;
    retirement.verify_cleanup(&entry.issuer, &entry.effect, &entry.cleanup)?;
    entry.cleaned = true;
    while journal.entries.first().is_some_and(|entry| entry.cleaned) {
        journal.entries.remove(0);
        journal.retired = journal.retired.checked_add(1).ok_or("sequence_exhausted")?;
    }
    set(source, revision, &journal)
}
