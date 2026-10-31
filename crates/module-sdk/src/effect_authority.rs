//! Optional pairing-owned admission contract for source-scoped effects.
//!
//! The selected effect owner must hold each ticket until its final local effect
//! or send completes. A source identifier is not a grant. This contract alone
//! does not authenticate a protocol-to-lifecycle join or implement a barrier.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SERVICE: &str = "amaterasu.pairing-effects/1";

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct Source {
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

impl Source {
    pub fn valid(&self) -> bool {
        self.version == 1
            && hex(&self.id, 32)
            && token(&self.host_epoch)
            && token(&self.session_id)
            && token(&self.stream_id)
            && self.generation != 0
            && self.generation == self.stream_generation
            && token(&self.device)
            && self.device_generation != 0
            && token(&self.effect_owner_epoch)
    }
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn token(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

/// Wire-compatible with input extension capability identities; feedback leases
/// accept only the client-bound direction.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCapability {
    pub schema: String,
    pub version: u16,
    pub direction: FeedbackDirection,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackDirection {
    ToClient,
}
impl FeedbackCapability {
    pub fn valid(&self) -> bool {
        !self.schema.is_empty()
            && self.schema.len() <= 128
            && self.schema.contains('.')
            && self
                .schema
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
            && self.version != 0
    }
}

/// Transport and negotiated capabilities selected by the authenticated protocol.
/// No caller-supplied field proves that the client negotiated a capability.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StreamScope {
    pub capabilities: BTreeSet<FeedbackCapability>,
    pub controller_slot: u16,
    pub transport_service: String,
    pub transport_stream: u64,
    pub transport_epoch: String,
}
impl StreamScope {
    pub fn valid(&self) -> bool {
        !self.capabilities.is_empty()
            && self.capabilities.len() <= 32
            && self.capabilities.iter().all(FeedbackCapability::valid)
            && self.transport_service.strip_suffix("/1").is_some_and(token)
            && self.transport_stream != 0
            && hex(&self.transport_epoch, 32)
    }
}

/// The selected effect owner attests that it received and verified this exact
/// whole-stream retirement receipt from the selected transport broker route.
/// The structure alone is not cryptographic proof or proof of remote delivery.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StreamRetirement {
    pub ok: bool,
    pub service: String,
    pub stream: u64,
    pub provider_epoch: String,
    pub operation_id: String,
    pub scope: String,
    pub state: String,
}
impl StreamRetirement {
    pub fn matches(&self, scope: &StreamScope) -> bool {
        self.ok
            && self.service == scope.transport_service
            && self.stream == scope.transport_stream
            && self.provider_epoch == scope.transport_epoch
            && token(&self.operation_id)
            && self.scope == "whole_control_stream"
            && self.state == "retired"
    }
}

/// Broker request shape for the optional, startup-selected authority service.
/// Bind is an assertion by the trusted protocol component, never client JSON.
#[derive(Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Recover {
        epoch: String,
        owner_epoch: String,
    },
    Bind {
        epoch: String,
        certificate_pem: String,
        source: Source,
    },
    BindStream {
        epoch: String,
        certificate_pem: String,
        source: Source,
        scope: StreamScope,
    },
    AdmitStreamLease {
        epoch: String,
        source: Source,
        operation_id: String,
        sequence: u64,
        scope: StreamScope,
    },
    LookupStreamLease {
        epoch: String,
        source: Source,
        operation_id: String,
        scope: StreamScope,
    },
    CompleteStreamLease {
        epoch: String,
        operation_id: String,
        ticket: String,
        retirement: StreamRetirement,
    },
    Admit {
        epoch: String,
        source: Source,
        operation_id: String,
        sequence: u64,
    },
    Status {},
    Drain {
        epoch: String,
        fence_generation: u64,
    },
    Lookup {
        epoch: String,
        operation_id: String,
    },
    Acknowledge {
        epoch: String,
        operation_id: String,
        ticket: String,
    },
    Complete {
        epoch: String,
        operation_id: String,
        ticket: String,
    },
}

/// Owner-side ticket lifecycle; does not authenticate a source or activate routes.
pub mod consumer;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stream_scope_requires_exact_client_bound_capability_and_provider_epoch() {
        let value = json!({"controller_slot":7,"capabilities":[{"schema":"org.example.feedback","version":1,"direction":"to_client"}],
            "transport_service":"amaterasu.control-transport/1","transport_stream":4,"transport_epoch":"a".repeat(32)});
        let scope: StreamScope = serde_json::from_value(value.clone()).unwrap();
        assert!(scope.valid());
        let mut wrong_direction = value.clone();
        wrong_direction["capabilities"][0]["direction"] = json!("to_device");
        assert!(serde_json::from_value::<StreamScope>(wrong_direction).is_err());
        let mut wrong_epoch = value.clone();
        wrong_epoch["transport_epoch"] = json!("a".repeat(31));
        assert!(
            !serde_json::from_value::<StreamScope>(wrong_epoch)
                .unwrap()
                .valid()
        );
        let mut wrong_version = value;
        wrong_version["capabilities"][0]["version"] = json!(0);
        assert!(
            !serde_json::from_value::<StreamScope>(wrong_version)
                .unwrap()
                .valid()
        );
    }
}
