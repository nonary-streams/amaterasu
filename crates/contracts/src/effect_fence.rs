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

/// Versioned identity of the selected provider and its complete installed API
/// schema. The authority validates its own semantic schema revision against
/// this exact method before arming an effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedEffectMethod {
    pub version: u8,
    pub provider: String,
    pub method: crate::ApiMethod,
    /// Host-issued identity of this exact provider instance, when supplied by
    /// the trusted host. Older, unslotted compositions may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_incarnation: Option<ProviderIncarnation>,
}

/// Opaque host-issued module-instance identity. It is a selector, not a grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIncarnation {
    pub version: u8,
    pub id: String,
}

/// A host assertion about the exact old effect and provider instance. It proves
/// only local worker retirement, never cleanup of an external effect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostProviderRetirement {
    pub version: u8,
    pub broker_epoch: String,
    pub provider_incarnation: ProviderIncarnation,
    pub effect: GuardedEffectIdentity,
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
    /// Opt in to bounded recyclable slots. Legacy source commands are denied
    /// and every new broker starts closed until authority recovery.
    #[serde(default, skip_serializing_if = "is_false")]
    pub source_slots: bool,
    pub methods: Vec<ProtectedMethod>,
    /// Only explicitly selected cleanup methods remain directly callable on a
    /// protected service, and still require normal API approvals and grants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_methods: Vec<ProtectedMethod>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// An anti-replay incarnation from the selected broker, not an authorization
/// credential. All fields must be returned unchanged with slot operations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSlotLease {
    pub slot: u8,
    pub incarnation: u64,
    pub broker_epoch: String,
    pub token: String,
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

/// Number of fixed source slots. An occupied deterministic slot applies
/// backpressure to colliding sources; no other source is evicted.
pub const SOURCE_SLOT_COUNT: usize = 64;

impl EffectSource {
    /// Select the stable slot from the lifecycle-issued lowercase hex ID.
    /// This is not evidence that the source or its other fields are valid.
    pub fn slot(&self) -> Option<u8> {
        if self.id.len() != 32
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        u8::from_str_radix(&self.id[30..32], 16)
            .ok()
            .map(|byte| byte % SOURCE_SLOT_COUNT as u8)
    }
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
    /// Authority-selected semantic revision; the authority must map it to the
    /// exact selected API schema supplied to `ArmSelected`.
    pub schema_revision: u32,
    /// Digest of the original protocol payload, verified by the authority.
    /// The broker independently compares the exact serialized API arguments.
    pub payload_sha256: String,
    pub payload: Value,
    pub timeout_ms: u64,
    pub operation_id: String,
    pub parent: Option<u64>,
}

/// Durable, payload-free identity for a protected call. The authority must
/// verify the digest and cleanup identity against its authenticated journal.
/// This structure cannot admit work or reconstruct a provider result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardedEffectIdentity {
    pub scope: EffectScope,
    pub sequence: u64,
    pub capability: String,
    pub provider: String,
    pub service: String,
    pub method: String,
    pub interface_major: u32,
    pub interface_minor: u32,
    pub schema_revision: u32,
    pub payload_sha256: String,
    pub timeout_ms: u64,
    pub operation_id: String,
    pub parent: Option<u64>,
}
impl GuardedEffect {
    /// Build journal-comparable metadata without retaining the request payload.
    pub fn identity(&self, provider: &str) -> GuardedEffectIdentity {
        GuardedEffectIdentity {
            scope: self.scope.clone(),
            sequence: self.sequence,
            capability: self.capability.clone(),
            provider: provider.to_owned(),
            service: self.service.clone(),
            method: self.method.clone(),
            interface_major: self.interface_major,
            interface_minor: self.interface_minor,
            schema_revision: self.schema_revision,
            payload_sha256: self.payload_sha256.clone(),
            timeout_ms: self.timeout_ms,
            operation_id: self.operation_id.clone(),
            parent: self.parent,
        }
    }
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
    /// Query only a protected method's registered issuer-callable selection.
    /// Available solely to the configured authority.
    SelectedMethod {
        method: ProtectedMethod,
    },
    /// Bind the authority's provider and full API schema to admission under the
    /// broker lock. Required when recyclable source slots are enabled.
    ArmSelected {
        effect: GuardedEffect,
        expected: Box<SelectedEffectMethod>,
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
    /// Read this broker's epoch; providers capture it before accepting work.
    BrokerEpoch {},
    /// Read the current broker epoch and, after recovery, one slot's lease.
    SlotStatus {
        slot: u8,
    },
    /// Each selected issuer/provider confirms its old workers and any local
    /// transport obligations are retired; the reserved host lifecycle endpoint
    /// separately confirms host-owned worker retirement. These authenticated
    /// acknowledgments bind to the new epoch and precede authority recovery.
    /// They do not prove persistent effects were cleaned up or remotely delivered.
    RecoveryRetired {
        broker_epoch: String,
    },
    /// Selected authority restores all 64 durable slot watermarks after it
    /// independently verifies old workers have retired. The boot epoch rejects stale recovery.
    RecoverSlots {
        broker_epoch: String,
        retired_incarnations: Vec<u64>,
    },
    /// Open only the deterministic slot for this source. The lease is obtained
    /// from SlotStatus and must have been durably reserved by the authority.
    OpenSlot {
        lease: SourceSlotLease,
        scope: EffectScope,
        retired_through: u64,
    },
    /// Open a recovery-only source after all old workers were authenticated as
    /// retired. It can be reconciled and closed, but never armed or dispatched.
    OpenRecoverySlot {
        lease: SourceSlotLease,
        scope: EffectScope,
        retired_through: u64,
        /// Issued watermark from the same durable snapshot. Every sequence
        /// above the retired watermark must be reconciled before source close.
        issued_through: u64,
    },
    /// Fence all exact effects for this source atomically and block new work.
    BeginCloseSlot {
        lease: SourceSlotLease,
        scope: EffectScope,
    },
    /// After beginning close, the selected issuer and the reserved host endpoint
    /// separately attest that the source's issuer worker is joined. Provider
    /// work still requires its own retirement acknowledgement.
    SourceRetired {
        lease: SourceSlotLease,
        scope: EffectScope,
    },
    /// After durable source/slot retirement and compaction, release the source
    /// and rotate its slot token. Before calling, the authority must durably
    /// persist the slot incarnation as retired, including on an uncertain reply.
    /// Never treat this as a worker-retirement proof.
    FinishCloseSlot {
        lease: SourceSlotLease,
        scope: EffectScope,
        retired_through: u64,
    },
    /// Authority-only payload-free fence and lookup, for durable journal
    /// recovery. The identity cannot dispatch a payload or manufacture a result.
    /// Even a retired status does not establish cleanup of persistent effects;
    /// the authority must verify those obligations before compaction or reuse.
    RecoverFence {
        identity: GuardedEffectIdentity,
    },
    LookupIdentity {
        identity: GuardedEffectIdentity,
    },
    /// The inner command must be Arm, Dispatch, Fence, Lookup, Compact,
    /// RecoverFence or LookupIdentity. The lease binds every operation to the
    /// exact broker/slot incarnation.
    SlotCommand {
        lease: SourceSlotLease,
        command: Box<EffectFenceCommand>,
    },
    /// Selected providers include the current broker epoch when confirming work
    /// retirement, so delayed acknowledgments cannot cross broker restarts.
    ProviderRetiredInEpoch {
        broker_epoch: String,
        request: u64,
    },
    /// Sent by the selected provider only after this request's final local work
    /// and all relevant owned workers/transport obligations have retired.
    ProviderRetired {
        request: u64,
    },
}
