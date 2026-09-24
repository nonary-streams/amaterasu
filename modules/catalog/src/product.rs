//! Durable provider ownership records kept beside, but separate from, user entries.
use crate::model::{ConfigOverrides, Entry, bounded_text, parse_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const ENTRY_PREFIX: &str = "~provider.entry:";
pub(crate) const STATE_PREFIX: &str = "~provider.state:";
pub(crate) const MAX_BATCH_ENTRIES: usize = 8;
pub(crate) const MAX_BATCH_BYTES: usize = 12_000;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Marker {
    pub(crate) version: u8,
    pub(crate) caller: String,
    pub(crate) source_id: String,
    pub(crate) entry_id: String,
    pub(crate) generation: u64,
    pub(crate) fingerprint: String,
    #[serde(default)]
    pub(crate) config_overrides: ConfigOverrides,
    pub(crate) detached: bool,
    #[serde(default)]
    pub(crate) remove_generation: Option<u64>,
    /// First time this provider imported the source row. Existing markers that
    /// predate timestamps stay unknown so TTL cleanup fails closed.
    #[serde(default)]
    pub(crate) added_at_unix: Option<i64>,
    /// Provider-defined source selection flags, kept outside the editable Entry.
    #[serde(default)]
    pub(crate) source_flags: u32,
    /// Reason for a complete-snapshot removal intent. `None` with a generation
    /// is the legacy explicit-uninstalled marker format.
    #[serde(default)]
    pub(crate) remove_reason: Option<RemovalReason>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemovalReason {
    Uninstalled,
    Expired,
    Selection,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotState {
    pub(crate) version: u8,
    pub(crate) generation: u64,
    pub(crate) sync_id: String,
    pub(crate) source_revision: u64,
    pub(crate) next_sequence: u64,
    pub(crate) last_batch: Option<CommittedBatch>,
    pub(crate) finished: bool,
    pub(crate) cleanup_pending: bool,
    pub(crate) finish_expected_revision: Option<u64>,
    #[serde(default = "default_remove_stale")]
    pub(crate) finish_remove_stale: bool,
    #[serde(default)]
    pub(crate) finish_remove_uninstalled: bool,
    #[serde(default)]
    pub(crate) finish_remove_expired: bool,
    #[serde(default)]
    pub(crate) finish_remove_selection: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommittedBatch {
    pub(crate) sequence: u64,
    pub(crate) expected_revision: u64,
    pub(crate) revision: u64,
    pub(crate) next_sequence: u64,
    pub(crate) fingerprint: String,
}

impl SnapshotState {
    pub(crate) fn initial() -> Self {
        Self {
            version: 1,
            generation: 0,
            sync_id: String::new(),
            source_revision: 0,
            next_sequence: 0,
            last_batch: None,
            finished: false,
            cleanup_pending: false,
            finish_expected_revision: None,
            finish_remove_stale: true,
            finish_remove_uninstalled: false,
            finish_remove_expired: false,
            finish_remove_selection: false,
        }
    }

    pub(crate) fn validate(&self) -> bool {
        (self.version == 1)
            && (self.generation == 0 || valid_sync_id(&self.sync_id))
            && (self.generation != 0 || (self.sync_id.is_empty() && self.next_sequence == 0))
            && (!self.finished || self.generation != 0)
            && (!self.cleanup_pending || self.finished)
            && (self.finish_expected_revision.is_some() || !self.finished)
            && (!self.finish_remove_uninstalled || self.finished)
            && (!self.finish_remove_expired || self.finished)
            && (!self.finish_remove_selection || self.finished)
            && (self.finished
                || !(self.finish_remove_uninstalled
                    || self.finish_remove_expired
                    || self.finish_remove_selection))
            && self.last_batch.as_ref().is_none_or(|batch| {
                batch.sequence.checked_add(1) == Some(batch.next_sequence)
                    && batch.next_sequence == self.next_sequence
                    && batch.fingerprint.len() == 64
                    && batch
                        .fingerprint
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
            })
            && (self.next_sequence == 0 || self.last_batch.is_some())
    }
}

fn default_remove_stale() -> bool {
    true
}

pub(crate) fn state_key(caller: &str) -> String {
    format!("{STATE_PREFIX}{caller}")
}

pub(crate) fn entry_key(caller: &str, source_id: &str) -> String {
    format!("{ENTRY_PREFIX}{caller}:{source_id}")
}

pub(crate) fn parse_entry_key(key: &str) -> Option<(&str, &str)> {
    let suffix = key.strip_prefix(ENTRY_PREFIX)?;
    let (caller, source_id) = suffix.split_once(':')?;
    (valid_principal(caller) && valid_source_id(source_id)).then_some((caller, source_id))
}

pub(crate) fn validate_marker(marker: &Marker, caller: &str, source_id: &str) -> bool {
    marker.version == 1
        && marker.caller == caller
        && marker.source_id == source_id
        && parse_id(&marker.entry_id).is_some()
        && marker.generation > 0
        && marker.fingerprint.len() == 64
        && marker
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && marker.config_overrides.validate().is_ok()
        && marker
            .remove_generation
            .is_none_or(|generation| generation > 0)
        && marker
            .added_at_unix
            .is_none_or(|timestamp| (0..=253_402_300_799).contains(&timestamp))
        && (marker.remove_generation.is_some() || marker.remove_reason.is_none())
        && (marker.remove_reason.is_none() || marker.remove_generation.is_some())
}

pub(crate) fn marker_value(marker: &Marker) -> Value {
    serde_json::to_value(marker).expect("provider marker serializes")
}

pub(crate) fn entry_fingerprint(entry: &Entry) -> String {
    let bytes = serde_json::to_vec(entry).expect("validated entry serializes");
    fingerprint_bytes(&bytes)
}

pub(crate) fn provider_entry_fingerprint(caller: &str, entry: &Entry) -> String {
    if caller == "amaterasu.playnite" {
        let mut provider_fields = entry.clone();
        provider_fields.config_overrides = ConfigOverrides::default();
        entry_fingerprint(&provider_fields)
    } else {
        entry_fingerprint(entry)
    }
}

pub(crate) fn fingerprint_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn valid_sync_id(value: &str) -> bool {
    bounded_text(value, 128) && !value.trim().is_empty()
}

pub(crate) fn valid_source_id(value: &str) -> bool {
    bounded_text(value, 96) && !value.trim().is_empty()
}

fn valid_principal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
