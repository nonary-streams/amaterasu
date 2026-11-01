//! Source-specific retirement requests for the pairing effect authority.
//!
//! A source fence is issued by the selected direct protocol or effect owner.
//! Completion is asserted by the selected direct effect owner after its local
//! producer and transport have retired. An assertion is not a sandbox or
//! remote-delivery proof.
use crate::effect_authority::{Source, StreamRetirement, StreamScope};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    FenceProtocolRestart {
        epoch: String,
        operation_id: String,
    },
    FenceSource {
        epoch: String,
        source: Source,
        scope: StreamScope,
        operation_id: String,
    },
    CompleteSourceRetirement {
        epoch: String,
        source: Source,
        scope: StreamScope,
        operation_id: String,
        retirement: Option<StreamRetirement>,
    },
    LookupSourceRetirement {
        epoch: String,
        source: Source,
        scope: StreamScope,
        operation_id: String,
    },
    LookupCurrentSourceRetirement {
        epoch: String,
        source: Source,
        scope: StreamScope,
    },
}

impl Request {
    pub fn valid(&self) -> bool {
        let (epoch, source_scope, operation_id) = match self {
            Self::FenceProtocolRestart {
                epoch,
                operation_id,
            } => (epoch, None, Some(operation_id)),
            Self::FenceSource {
                epoch,
                source,
                scope,
                operation_id,
            }
            | Self::CompleteSourceRetirement {
                epoch,
                source,
                scope,
                operation_id,
                ..
            }
            | Self::LookupSourceRetirement {
                epoch,
                source,
                scope,
                operation_id,
            } => (epoch, Some((source, scope)), Some(operation_id)),
            Self::LookupCurrentSourceRetirement {
                epoch,
                source,
                scope,
            } => (epoch, Some((source, scope)), None),
        };
        epoch.len() == 32
            && epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && source_scope.is_none_or(|(source, scope)| source.valid() && scope.valid())
            && operation_id.is_none_or(|value| {
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
            })
    }

    /// Calls are direct: the broker derives caller and initiator from this owner.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
    ) -> crate::ModuleResult<u64> {
        if !self.valid() {
            return Err("invalid_source_retirement".into());
        }
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: crate::effect_authority::SERVICE,
                timeout_ms,
                operation_id,
                parent: None,
            },
            self,
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Fenced,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolFenceReceipt {
    pub ok: bool,
    pub epoch: String,
    pub operation_id: String,
    pub state: State,
    pub fence_generation: u64,
}

impl ProtocolFenceReceipt {
    pub fn matches(&self, epoch: &str, operation_id: &str) -> bool {
        self.ok
            && self.epoch == epoch
            && self.operation_id == operation_id
            && self.state == State::Fenced
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub ok: bool,
    pub epoch: String,
    pub source: Source,
    pub scope: StreamScope,
    pub operation_id: String,
    pub state: State,
}

impl Receipt {
    pub fn matches_source(&self, epoch: &str, source: &Source, scope: &StreamScope) -> bool {
        self.ok
            && self.epoch == epoch
            && &self.source == source
            && &self.scope == scope
            && !self.operation_id.is_empty()
            && self.operation_id.len() <= 128
            && !self.operation_id.chars().any(char::is_control)
    }
    pub fn matches(
        &self,
        epoch: &str,
        source: &Source,
        scope: &StreamScope,
        operation_id: &str,
    ) -> bool {
        self.ok
            && self.epoch == epoch
            && &self.source == source
            && &self.scope == scope
            && self.operation_id == operation_id
    }
}
