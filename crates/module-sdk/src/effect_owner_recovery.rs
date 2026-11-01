//! Selected effect-owner restart fence and stable recovery enumeration.
//! Records are identities to reconcile, not evidence of transport retirement.
use crate::effect_authority::{Source, StreamRetirement, StreamScope};
use serde::{Deserialize, Serialize};

/// Upper bound on the pairing ledger's sources and retirement tombstones.
pub const MAX_RECORDS: u32 = 4160;

fn required_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    FenceEffectOwnerRestart {
        version: u8,
        epoch: String,
        owner_epoch: String,
        operation_id: String,
    },
    ReadEffectOwnerRestart {
        version: u8,
        epoch: String,
        owner_epoch: String,
        operation_id: String,
        index: u32,
    },
    ConfirmEffectOwnerRestart {
        version: u8,
        epoch: String,
        owner_epoch: String,
        operation_id: String,
        index: u32,
        retirement: StreamRetirement,
    },
}

impl Request {
    pub fn valid(&self) -> bool {
        let (version, epoch, owner_epoch, operation_id, index) = match self {
            Self::FenceEffectOwnerRestart {
                version,
                epoch,
                owner_epoch,
                operation_id,
            } => (*version, epoch, owner_epoch, operation_id, None),
            Self::ReadEffectOwnerRestart {
                version,
                epoch,
                owner_epoch,
                operation_id,
                index,
            }
            | Self::ConfirmEffectOwnerRestart {
                version,
                epoch,
                owner_epoch,
                operation_id,
                index,
                ..
            } => (*version, epoch, owner_epoch, operation_id, Some(*index)),
        };
        version == 1
            && epoch.len() == 32
            && epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && token(owner_epoch)
            && token(operation_id)
            && index.is_none_or(|index| index < MAX_RECORDS)
    }

    /// The broker supplies the selected direct owner's identity.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        request_id: &str,
    ) -> crate::ModuleResult<u64> {
        if !self.valid() {
            return Err("invalid_owner_restart".into());
        }
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: crate::effect_authority::SERVICE,
                timeout_ms,
                operation_id: request_id,
                parent: None,
            },
            self,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub operation_id: String,
    pub sequence: u64,
    pub ticket: String,
    pub complete: bool,
    #[serde(deserialize_with = "required_optional")]
    pub retirement: Option<StreamRetirement>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub source: Source,
    pub scope: StreamScope,
    #[serde(deserialize_with = "required_optional")]
    pub lease: Option<Lease>,
    /// Retained after lease acknowledgement for exact transport reconciliation.
    #[serde(deserialize_with = "required_optional")]
    pub transport_retirement: Option<StreamRetirement>,
    #[serde(deserialize_with = "required_optional")]
    pub source_retirement: Option<SourceRetirement>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceRetirement {
    pub operation_id: String,
    pub complete: bool,
    #[serde(deserialize_with = "required_optional")]
    pub retirement: Option<StreamRetirement>,
}

impl SourceRecord {
    fn valid_for(&self, owner_epoch: &str) -> bool {
        self.source.valid()
            && self.source.effect_owner_epoch == owner_epoch
            && self.scope.valid()
            && self
                .transport_retirement
                .as_ref()
                .is_none_or(|receipt| receipt.matches(&self.scope))
            && self
                .lease
                .as_ref()
                .and_then(|lease| lease.retirement.as_ref())
                .is_none_or(|receipt| self.transport_retirement.as_ref() == Some(receipt))
            && self.source_retirement.as_ref().is_none_or(|retirement| {
                token(&retirement.operation_id)
                    && (retirement.complete || retirement.retirement.is_none())
                    && retirement
                        .retirement
                        .as_ref()
                        .is_none_or(|receipt| receipt.matches(&self.scope))
            })
            && self.lease.as_ref().is_none_or(|lease| {
                token(&lease.operation_id)
                    && lease.sequence != 0
                    && lease.ticket.len() == 32
                    && lease
                        .ticket
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && match (&lease.retirement, lease.complete) {
                        (Some(receipt), true) => receipt.matches(&self.scope),
                        (None, false) => true,
                        _ => false,
                    }
            })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FenceReceipt {
    pub ok: bool,
    pub version: u8,
    pub epoch: String,
    pub owner_epoch: String,
    pub operation_id: String,
    pub fence_generation: u64,
    pub count: u32,
}

impl FenceReceipt {
    pub fn matches(&self, epoch: &str, owner_epoch: &str, operation_id: &str) -> bool {
        self.ok
            && self.version == 1
            && self.epoch == epoch
            && self.owner_epoch == owner_epoch
            && self.operation_id == operation_id
            && self.fence_generation != 0
            && self.count <= MAX_RECORDS
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub ok: bool,
    pub version: u8,
    pub epoch: String,
    pub owner_epoch: String,
    pub operation_id: String,
    pub index: u32,
    pub count: u32,
    pub record: SourceRecord,
}

impl Page {
    pub fn matches(&self, fence: &FenceReceipt, index: u32) -> bool {
        self.ok
            && self.version == 1
            && self.epoch == fence.epoch
            && self.owner_epoch == fence.owner_epoch
            && self.operation_id == fence.operation_id
            && self.count == fence.count
            && self.count <= MAX_RECORDS
            && index < self.count
            && self.index == index
            && self.record.valid_for(&self.owner_epoch)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationReceipt {
    pub ok: bool,
    pub version: u8,
    pub epoch: String,
    pub owner_epoch: String,
    pub operation_id: String,
    pub index: u32,
}

impl ConfirmationReceipt {
    pub fn matches(&self, fence: &FenceReceipt, index: u32) -> bool {
        self.ok
            && self.version == 1
            && self.epoch == fence.epoch
            && self.owner_epoch == fence.owner_epoch
            && self.operation_id == fence.operation_id
            && self.index == index
            && index < fence.count
            && fence.count <= MAX_RECORDS
    }
}
