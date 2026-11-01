//! Optional pairing-owned admission contract for source-scoped effects.
//!
//! The selected effect owner must hold each ticket until its final local effect
//! or send completes. A source identifier is not a grant. This contract alone
//! does not authenticate a protocol-to-lifecycle join or implement a barrier.
use crate::{input::extensions::source_label::SourceLabel, transport::control_retirement};
use amaterasu_extension_api::LifecycleTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const SERVICE: &str = "amaterasu.pairing-effects/1";
pub const ADMINISTRATION_SERVICE: &str = "amaterasu.pairing/1";

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
    /// Construct the full effect source from trusted lifecycle data and the
    /// independently resolved device and owner incarnation. This is a checked
    /// conversion, not source attestation or admission authority.
    pub fn from_lifecycle(
        label: &SourceLabel,
        target: &LifecycleTarget,
        device: String,
        device_generation: u64,
        effect_owner_epoch: String,
    ) -> Result<Self, &'static str> {
        let LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } = target
        else {
            return Err("invalid_source");
        };
        if !label.valid_for(*generation) {
            return Err("invalid_source");
        }
        let source = Self {
            version: u8::try_from(label.version).map_err(|_| "invalid_source")?,
            id: label.id.clone(),
            generation: label.generation,
            host_epoch: host_epoch.clone(),
            session_id: session_id.clone(),
            stream_id: stream_id.clone(),
            stream_generation: *generation,
            device,
            device_generation,
            effect_owner_epoch,
        };
        source.valid().then_some(source).ok_or("invalid_source")
    }

    /// Extract a checked lifecycle label and stream target. These values are
    /// selectors only; the trusted owner must authenticate them separately.
    pub fn to_lifecycle(&self) -> Result<(SourceLabel, LifecycleTarget), &'static str> {
        if !self.valid() {
            return Err("invalid_source");
        }
        Ok((
            SourceLabel {
                version: u32::from(self.version),
                id: self.id.clone(),
                generation: self.generation,
            },
            LifecycleTarget::Stream {
                host_epoch: self.host_epoch.clone(),
                session_id: self.session_id.clone(),
                stream_id: self.stream_id.clone(),
                generation: self.stream_generation,
            },
        ))
    }

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

// These checks inspect wire values only. The caller must obtain responses from
// the selected authenticated authority endpoint and enforce its own state machine.
pub(crate) fn response_matches(response: &Value, epoch: &str, operation_id: &str) -> bool {
    response.get("ok").and_then(Value::as_bool) == Some(true)
        && response.get("epoch").and_then(Value::as_str) == Some(epoch)
        && response.get("operation_id").and_then(Value::as_str) == Some(operation_id)
}

pub(crate) fn response_ticket<'a>(
    response: &'a Value,
    epoch: &str,
    operation_id: &str,
    lookup: bool,
) -> Option<(&'a str, bool)> {
    if !response_matches(response, epoch, operation_id) {
        return None;
    }
    let ticket = response.get("ticket")?.as_str()?;
    if !hex(ticket, 32) {
        return None;
    }
    let fenced = if lookup {
        response.get("fenced")?.as_bool()?
    } else {
        if response.get("fenced").is_some() {
            return None;
        }
        false
    };
    Some((ticket, fenced))
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

/// A trusted startup policy selects a specific existing paired-client permission
/// for each supported feedback schema. Capability advertisement is not a grant.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackPermission {
    View,
    Launch,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackPolicy {
    pub capability: FeedbackCapability,
    pub required_permission: FeedbackPermission,
}

/// An optional protocol effect must be selected by trusted composition and
/// explicitly granted to the paired client. Its API route is selected by the
/// host; this name alone never selects a provider or proves negotiation.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(transparent)]
pub struct ProtocolCapability(pub String);
impl ProtocolCapability {
    pub fn valid(&self) -> bool {
        amaterasu_contracts::valid_name(&self.0)
    }
}

/// Trusted startup binds an optional capability to one exact selected API and
/// provider. This policy supplements, rather than replaces, the client grant.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolEffectPolicy {
    pub capability: ProtocolCapability,
    pub provider: String,
    pub message_type: u16,
    pub method: crate::extension_api::MethodRef,
    pub cleanup: crate::extension_api::MethodRef,
    pub max_payload_bytes: usize,
    pub required_permission: FeedbackPermission,
}

impl ProtocolEffectPolicy {
    pub fn valid(&self) -> bool {
        self.capability.valid()
            && crate::protocol::valid_extension_message_type(self.message_type)
            && amaterasu_contracts::valid_name(&self.provider)
            && amaterasu_contracts::valid_name(&self.method.service)
            && amaterasu_contracts::valid_name(&self.method.method)
            && self.method.interface_major != 0
            && self.method.schema_revision > 0
            && u32::try_from(self.method.schema_revision).is_ok()
            && amaterasu_contracts::valid_name(&self.cleanup.service)
            && amaterasu_contracts::valid_name(&self.cleanup.method)
            && self.cleanup.interface_major != 0
            && self.cleanup.schema_revision > 0
            && (self.cleanup.service != self.method.service
                || self.cleanup.method != self.method.method
                || self.cleanup.interface_major != self.method.interface_major)
            && self.max_payload_bytes > 0
            && self.max_payload_bytes <= crate::protocol::MAX_MESSAGE_BYTES
    }
}

/// Administrator-only pairing operation. The certificate reference belongs to
/// the existing private pairing administration API; it is never sent to clients
/// or the effect provider. Empty grants revoke the selected optional scopes.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdministrationRequest {
    UpdateClientFeedbackGrants {
        certificate_id: String,
        grants: BTreeSet<FeedbackCapability>,
        expected_revision: u64,
    },
    UpdateClientProtocolGrants {
        certificate_id: String,
        grants: BTreeSet<ProtocolCapability>,
        expected_revision: u64,
    },
}
impl AdministrationRequest {
    /// Submit to the existing pairing administrator service. The broker supplies
    /// the caller identity; acceptance does not confirm that the update committed.
    pub fn submit(
        &self,
        runtime: &crate::RuntimeClient,
        timeout_ms: u64,
        operation_id: &str,
        parent: Option<u64>,
    ) -> crate::ModuleResult<u64> {
        self.validate().map_err(str::to_owned)?;
        runtime.request_serialized(
            crate::requests::RequestOptions {
                service: ADMINISTRATION_SERVICE,
                timeout_ms,
                operation_id,
                parent,
            },
            self,
        )
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::UpdateClientFeedbackGrants {
                certificate_id,
                grants,
                ..
            } if certificate_id.len() == 64
                && certificate_id.bytes().all(|b| b.is_ascii_hexdigit())
                && grants.len() <= 32
                && grants.iter().all(FeedbackCapability::valid) =>
            {
                Ok(())
            }
            Self::UpdateClientProtocolGrants {
                certificate_id,
                grants,
                ..
            } if certificate_id.len() == 64
                && certificate_id.bytes().all(|b| b.is_ascii_hexdigit())
                && grants.len() <= 32
                && grants.iter().all(ProtocolCapability::valid) =>
            {
                Ok(())
            }
            _ => Err("invalid_request"),
        }
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
        self.service == scope.transport_service
            && token(&self.operation_id)
            && control_retirement::matches_retired(
                self.ok,
                &self.state,
                &self.scope,
                (self.stream, &self.provider_epoch),
                (scope.transport_stream, &scope.transport_epoch),
            )
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
    /// Selected protocol owner attests negotiated capabilities for an already
    /// authenticated lifecycle source. This is not a client grant.
    AttestProtocolCapabilities {
        version: u8,
        epoch: String,
        source: Source,
        capabilities: BTreeSet<ProtocolCapability>,
    },
    /// Ask the selected authority to durably admit an exact protocol effect.
    /// Unsupported hosts deny this operation without granting a ticket.
    AdmitProtocolEffect {
        version: u8,
        epoch: String,
        source: Source,
        effect: Box<crate::protocol::PendingEffect>,
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
    fn lifecycle_source_conversion_preserves_wire_fields_and_checks_incarnations() {
        let label = SourceLabel {
            version: 1,
            id: "a".repeat(32),
            generation: 3,
        };
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 3,
        };
        let source =
            Source::from_lifecycle(&label, &target, "device".into(), 9, "owner".into()).unwrap();
        let expected = json!({"version":1,"id":"a".repeat(32),"generation":3,"host_epoch":"host", "session_id":"session", "stream_id":"stream", "stream_generation":3, "device":"device", "device_generation":9, "effect_owner_epoch":"owner"});
        assert_eq!(serde_json::to_value(&source).unwrap(), expected);
        assert_eq!(serde_json::from_value::<Source>(expected).unwrap(), source);
        let (observed_label, observed_target) = source.to_lifecycle().unwrap();
        assert_eq!(observed_label, label);
        assert_eq!(observed_target, target);
        assert_eq!(
            Source::from_lifecycle(
                &observed_label,
                &observed_target,
                "device".into(),
                9,
                "owner".into()
            )
            .unwrap(),
            source
        );
        let wrong_targets = [
            LifecycleTarget::Stream {
                host_epoch: "other".into(),
                session_id: "session".into(),
                stream_id: "stream".into(),
                generation: 3,
            },
            LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "other".into(),
                stream_id: "stream".into(),
                generation: 3,
            },
            LifecycleTarget::Stream {
                host_epoch: "host".into(),
                session_id: "session".into(),
                stream_id: "other".into(),
                generation: 3,
            },
        ];
        for wrong in wrong_targets {
            assert_ne!(observed_target, wrong);
        }
        let mut bad = label.clone();
        for version in [0, 2, 256] {
            bad.version = version;
            assert!(
                Source::from_lifecycle(&bad, &target, "device".into(), 9, "owner".into()).is_err()
            );
        }
        bad = label.clone();
        bad.generation = 4;
        assert!(Source::from_lifecycle(&bad, &target, "device".into(), 9, "owner".into()).is_err());
        bad = label.clone();
        bad.id = "A".repeat(32);
        assert!(Source::from_lifecycle(&bad, &target, "device".into(), 9, "owner".into()).is_err());
        assert!(
            Source::from_lifecycle(&label, &target, "device".into(), 0, "owner".into()).is_err()
        );
        assert!(
            Source::from_lifecycle(&label, &target, "device".into(), 9, String::new()).is_err()
        );
        let mut invalid_source = source;
        invalid_source.stream_generation += 1;
        assert!(invalid_source.to_lifecycle().is_err());
    }

    #[test]
    fn shared_retirement_validation_preserves_exact_operation_and_service_checks() {
        use crate::transport::control_retirement::Receipt;
        let epoch = "a".repeat(32);
        let scope = StreamScope {
            capabilities: [FeedbackCapability {
                schema: "org.example.feedback".into(),
                version: 1,
                direction: FeedbackDirection::ToClient,
            }]
            .into(),
            controller_slot: 1,
            transport_service: "example.transport/1".into(),
            transport_stream: 5,
            transport_epoch: epoch.clone(),
        };
        let mut authority_receipt = StreamRetirement {
            ok: true,
            service: scope.transport_service.clone(),
            stream: 5,
            provider_epoch: epoch.clone(),
            operation_id: "retire".into(),
            scope: "whole_control_stream".into(),
            state: "retired".into(),
        };
        let wire = serde_json::to_value(&authority_receipt).unwrap();
        assert_eq!(
            serde_json::from_value::<StreamRetirement>(wire.clone()).unwrap(),
            authority_receipt
        );
        let mut transport_wire = wire;
        transport_wire.as_object_mut().unwrap().remove("service");
        let receipt: Receipt = serde_json::from_value(transport_wire.clone()).unwrap();
        assert!(receipt.matches(5, "retire", &epoch));
        assert!(!receipt.matches(5, "different", &epoch));
        assert!(!receipt.matches(6, "retire", &epoch));
        assert!(!receipt.matches(5, "retire", "other"));
        assert!(authority_receipt.matches(&scope));
        authority_receipt.service = "other.transport/1".into();
        assert!(!authority_receipt.matches(&scope));
        authority_receipt.service = scope.transport_service.clone();
        authority_receipt.operation_id.clear();
        assert!(!authority_receipt.matches(&scope));
        for (field, value) in [
            ("ok", json!(false)),
            ("state", json!("open")),
            ("scope", json!("source")),
            ("stream", json!(6)),
            ("provider_epoch", json!("other")),
        ] {
            let mut changed = transport_wire.clone();
            changed[field] = value;
            let invalid: Receipt = serde_json::from_value(changed.clone()).unwrap();
            assert!(!invalid.matches(5, "retire", &epoch));
            changed["service"] = json!(scope.transport_service);
            let invalid: StreamRetirement = serde_json::from_value(changed).unwrap();
            assert!(!invalid.matches(&scope));
        }
    }

    #[test]
    fn both_authority_clients_reject_mismatched_response_identity() {
        use super::consumer::{EffectTicket, State};
        use crate::protocol_authority::EffectOperation;
        let label = SourceLabel {
            version: 1,
            id: "a".repeat(32),
            generation: 1,
        };
        let target = LifecycleTarget::Stream {
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            generation: 1,
        };
        let source =
            Source::from_lifecycle(&label, &target, "device".into(), 1, "owner".into()).unwrap();
        let epoch = "e".repeat(32);
        let protocol =
            EffectOperation::new(epoch.clone(), source.clone(), "caller", "operation", 1).unwrap();
        let Request::Admit {
            operation_id: protocol_id,
            ..
        } = protocol.admit()
        else {
            unreachable!()
        };
        let protocol_response =
            json!({"ok":true,"epoch":epoch,"operation_id":protocol_id,"ticket":"b".repeat(32)});
        let owner_response =
            json!({"ok":true,"epoch":epoch,"operation_id":"operation","ticket":"b".repeat(32)});
        let mut owner = EffectTicket::new(epoch, source, "operation".into(), 1).unwrap();
        for (field, value) in [
            ("ok", json!(false)),
            ("epoch", json!("wrong")),
            ("operation_id", json!("wrong")),
            ("ticket", json!("bad")),
            ("fenced", json!(false)),
        ] {
            let mut bad = protocol_response.clone();
            bad[field] = value.clone();
            assert!(protocol.accepted_admission(&bad).is_err());
            let mut bad = owner_response.clone();
            bad[field] = value;
            assert!(owner.accept_admission(&bad).is_err());
            assert_eq!(owner.state(), State::AwaitAdmission);
        }
        let mut wrong_kind = owner_response.clone();
        wrong_kind["kind"] = json!("stream_lease");
        assert!(owner.accept_admission(&wrong_kind).is_err());
        assert!(protocol.accepted_admission(&protocol_response).is_ok());
        owner.accept_admission(&owner_response).unwrap();
        assert_eq!(owner.state(), State::Ready);
    }

    #[test]
    fn authority_response_common_checks_reject_bad_identity_ticket_and_fencing() {
        let response =
            json!({"ok":true,"epoch":"epoch", "operation_id":"operation", "ticket":"b".repeat(32)});
        assert!(response_ticket(&response, "epoch", "operation", false).is_some());
        assert!(response_ticket(&response, "epoch", "operation", true).is_none());
        for (field, value) in [
            ("ok", json!(false)),
            ("epoch", json!("other")),
            ("operation_id", json!("other")),
            ("ticket", json!("B".repeat(32))),
            ("ticket", json!("b".repeat(31))),
        ] {
            let mut invalid = response.clone();
            invalid[field] = value;
            assert!(response_ticket(&invalid, "epoch", "operation", false).is_none());
        }
        for value in [json!(true), json!(false), json!(null), json!("false")] {
            let mut lookup = response.clone();
            lookup["fenced"] = value.clone();
            assert!(response_ticket(&lookup, "epoch", "operation", false).is_none());
            assert_eq!(
                response_ticket(&lookup, "epoch", "operation", true).map(|(_, fenced)| fenced),
                value.as_bool()
            );
        }
        assert!(!response_matches(
            &json!({"ok":true,"epoch":"epoch","operation_id":"other"}),
            "epoch",
            "operation"
        ));
    }

    #[test]
    fn administration_request_is_bounded_and_never_supplies_effect_authority() {
        let request = json!({"action":"update_client_feedback_grants","certificate_id":"a".repeat(64),
            "expected_revision":7,"grants":[{"schema":"speaker.pcm","version":1,"direction":"to_client"}]});
        let parsed: AdministrationRequest = serde_json::from_value(request.clone()).unwrap();
        assert!(parsed.validate().is_ok());
        let mut forged = request.clone();
        forged["caller"] = json!("administrator");
        assert!(serde_json::from_value::<AdministrationRequest>(forged).is_err());
        let mut invalid = request;
        invalid["grants"][0]["version"] = json!(0);
        assert!(
            serde_json::from_value::<AdministrationRequest>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }

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
