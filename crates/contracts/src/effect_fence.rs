//! Opt-in broker fence for an authority-owned, per-source effect journal.
//!
//! Source and epoch fields are selectors, not credentials. Trusted startup
//! chooses the authority and issuer; normal API grants remain mandatory.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedMethod {
    pub service: String,
    pub method: String,
    pub interface_major: u32,
}

/// Trusted startup policy. The authority must reconcile durable State and retire
/// old issuer/provider workers before opening a source after broker restart.
/// Authority, issuer and selected provider must be distinct principals. All
/// broker services granted to a protected provider are closed to direct calls,
/// except selected cleanup API methods. Native paths remain trusted composition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectFencePolicy {
    pub authority: String,
    pub issuer: String,
    pub methods: Vec<ProtectedMethod>,
    /// Only explicitly selected cleanup methods remain directly callable on a
    /// protected service, and still require normal API approvals and grants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_methods: Vec<ProtectedMethod>,
}

/// Exact lifecycle and effect-owner incarnation, authenticated by the authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectSource {
    pub version: u8,
    pub id: String,
    pub generation: u64,
    pub host_epoch: String,
    pub session_id: String,
    pub stream_id: String,
    pub stream_generation: u64,
    pub device: String,
    pub device_generation: u64,
    pub effect_owner_epoch: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectScope {
    pub authority_epoch: String,
    pub issuer_epoch: String,
    pub source: EffectSource,
}

/// The authority arms the exact request. The broker compares all fields on
/// dispatch; issuer, provider and initiator come from trusted runtime state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardedEffect {
    pub scope: EffectScope,
    /// Nonzero sequence from this source's durable journal, never the
    /// authority's independent global sequence.
    pub sequence: u64,
    pub capability: String,
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    /// Authority-selected schema revision; the installed API schema is also
    /// validated by the broker during admission.
    pub schema_revision: u32,
    /// Digest of the original protocol payload, verified by the authority.
    /// The broker independently compares the exact serialized API arguments.
    pub payload_sha256: String,
    pub payload: Value,
    pub timeout_ms: u64,
    pub operation_id: String,
    pub parent: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectFenceCommand {
    /// This is a trusted recovery assertion, not a State read or elapsed timeout.
    /// The authority must have fenced and retired all prior issuer/provider work,
    /// reconciled durable state, and chosen a fresh issuer epoch before opening.
    OpenSource {
        scope: EffectScope,
        retired_through: u64,
    },
    /// After durably retaining retirement evidence, compact only a retired prefix.
    Compact {
        scope: EffectScope,
        retired_through: u64,
    },
    Arm {
        effect: GuardedEffect,
    },
    Dispatch {
        effect: GuardedEffect,
    },
    /// Stops new admission immediately; `Ok` is not a retirement receipt.
    Fence {
        effect: GuardedEffect,
    },
    /// Returns an exact matching `armed`, `admitted`, `fenced`, or `retired`
    /// status, or `unknown`. After compaction, `retired_sequence` only means
    /// the sequence cannot be admitted; it is not an exact receipt or result.
    /// At most 32 un-compacted sequences and 64 sources are retained.
    Lookup {
        effect: GuardedEffect,
    },
    /// Sent by the selected provider only after this request's final local work
    /// and all relevant owned workers/transport obligations have retired.
    ProviderRetired {
        request: u64,
    },
}
