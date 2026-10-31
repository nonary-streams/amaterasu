//! Owner-authenticated resource-control endpoint adapter.
//!
//! The embedding coordinator must supply a backend reconstructed from its
//! current durable graph, output and admission state. A permit or request field
//! alone never supplies this context. No permissive backend is installed here.
use amaterasu_extension_api::{
    LifecycleTarget, ResourceControlRequest, SchemaRef, validate_resource_control_request,
};
use amaterasu_module_sdk::ModuleResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::adapter::ParticipantSettings;
use crate::prepare_authority::{ControlContext, ControlPermitUse};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    operation: Operation,
    request: ResourceControlRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    #[serde(rename = "describe_resource_control")]
    Describe,
    #[serde(rename = "verify_resource_control")]
    Verify,
    #[serde(rename = "validate_resource_control")]
    Validate,
}

/// A snapshot reconstructed from trusted frozen policy and live journal state.
/// Resolver must require the active exact target and parent generation,
/// current resource and rights, and all required consumer retirements.
pub(super) struct ResolvedControl {
    pub participant: ParticipantSettings,
    pub context: ControlContext,
    /// Read from the coordinator's canonical session record.
    pub session_target: LifecycleTarget,
    pub resource_generation: u64,
    /// True only after the exact affected stream's consumers have retired.
    pub consumers_retired: bool,
}

/// Trusted coordinator implementation. It must re-read current state for each
/// call; enforce installed owner and method approval, exact frozen graph,
/// authenticated producer output, active admission/locks and retirement. The
/// schema validator resolves immutable signed content by digest, and performs
/// semantic validation rather than merely checking the schema reference.
/// `consume` must durably consume the exact bound permit before Execute.
/// `validate` is read-only and requires that same permit to be consumed,
/// unsettled, and still valid immediately before the native effect.
pub(super) trait ControlEndpointBackend {
    fn resolve(&mut self, request: &ResourceControlRequest) -> ModuleResult<ResolvedControl>;
    fn validate_arguments(&mut self, schema: &SchemaRef, arguments: &Value) -> ModuleResult<()>;
    /// Verify an unconsumed permit without creating effect authority.
    fn describe(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<()>;
    fn consume(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<ControlPermitUse>;
    fn validate(
        &mut self,
        owner: &str,
        resolved: &ResolvedControl,
        request: &ResourceControlRequest,
    ) -> ModuleResult<()>;
}

/// `caller` is supplied by the broker, not by the message payload. Failures use
/// one generic response; no native effect is authorized by malformed, stale or
/// unavailable trusted data. The backend must preserve durable effect fencing.
pub(super) fn control_endpoint(
    caller: &str,
    payload: Value,
    backend: &mut impl ControlEndpointBackend,
) -> Value {
    match authorize(caller, payload, backend) {
        Ok((status, resolved)) => {
            json!({"status":status, "session_target":resolved.session_target, "target":resolved.context.target, "output":resolved.context.output, "resource_generation":resolved.resource_generation, "consumers_retired":resolved.consumers_retired})
        }
        Err(_) => json!({"status":"rejected","reason":"resource_control_not_authorized"}),
    }
}

fn authorize(
    caller: &str,
    payload: Value,
    backend: &mut impl ControlEndpointBackend,
) -> ModuleResult<(&'static str, ResolvedControl)> {
    let envelope: Envelope =
        serde_json::from_value(payload).map_err(|_| "invalid_control_envelope")?;
    let request = &envelope.request;
    validate_resource_control_request(request)?;
    if caller.is_empty() || caller != request.proposal.owner_provider {
        return Err("owner_identity_mismatch".into());
    }
    let resolved = backend.resolve(request)?;
    let context = &resolved.context;
    if !matches!(&resolved.session_target, LifecycleTarget::Session { host_epoch, session_id, generation } if *generation > 0 && matches!(&context.target, LifecycleTarget::Stream { host_epoch: stream_epoch, session_id: stream_session, .. } if host_epoch == stream_epoch && session_id == stream_session) && context.parent_generation == Some(*generation))
    {
        return Err("control_session_mismatch".into());
    }
    if caller != context.policy.owner_provider
        || resolved.participant.provider != context.policy.dependent_provider
        || context.target != request.target
        || context.transition_id != request.transition_id
        || context.output.output_id != request.proposal.output_id
        || context.output.producer != caller
        || context.output.producer_service != context.policy.owner_service
        || context.output.resource != request.proposal.resource
        || context.policy.owner_service != request.proposal.owner_service
        || context.policy.owner_method != request.proposal.method
        || context.policy.argument_schema != request.proposal.argument_schema
    {
        return Err("control_context_mismatch".into());
    }
    backend.validate_arguments(&context.policy.argument_schema, &request.proposal.arguments)?;
    let status = match envelope.operation {
        Operation::Describe => {
            backend.describe(caller, &resolved, request)?;
            "described"
        }
        Operation::Verify => match backend.consume(caller, &resolved, request)? {
            ControlPermitUse::Execute => "execute",
            ControlPermitUse::ReattachOnly => "reattach_only",
        },
        Operation::Validate => {
            backend.validate(caller, &resolved, request)?;
            "valid"
        }
    };
    Ok((status, resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Backend {
        resolved: ResolvedControl,
        schema_available: bool,
        consumed: bool,
        rights: bool,
        consume_calls: usize,
    }
    impl ControlEndpointBackend for Backend {
        fn resolve(&mut self, _: &ResourceControlRequest) -> ModuleResult<ResolvedControl> {
            if !self.rights {
                return Err("stale".into());
            }
            Ok(ResolvedControl {
                participant: self.resolved.participant.clone(),
                context: self.resolved.context.clone(),
                session_target: self.resolved.session_target.clone(),
                resource_generation: self.resolved.resource_generation,
                consumers_retired: self.resolved.consumers_retired,
            })
        }
        fn validate_arguments(&mut self, _: &SchemaRef, arguments: &Value) -> ModuleResult<()> {
            if self.schema_available && arguments == &json!({"width":1920}) {
                Ok(())
            } else {
                Err("schema unavailable or invalid".into())
            }
        }
        fn consume(
            &mut self,
            _: &str,
            _: &ResolvedControl,
            _: &ResourceControlRequest,
        ) -> ModuleResult<ControlPermitUse> {
            self.consume_calls += 1;
            if self.consumed {
                Ok(ControlPermitUse::ReattachOnly)
            } else {
                self.consumed = true;
                Ok(ControlPermitUse::Execute)
            }
        }
        fn describe(
            &mut self,
            _: &str,
            _: &ResolvedControl,
            _: &ResourceControlRequest,
        ) -> ModuleResult<()> {
            if self.consumed {
                Err("already consumed".into())
            } else {
                Ok(())
            }
        }
        fn validate(
            &mut self,
            _: &str,
            _: &ResolvedControl,
            _: &ResourceControlRequest,
        ) -> ModuleResult<()> {
            if self.consumed {
                Ok(())
            } else {
                Err("not consumed".into())
            }
        }
    }
    fn fixture() -> (Backend, Value) {
        let method = json!({"service":"owner/2","method":"control","interface_major":2,"interface_minor":0,"schema_revision":1});
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        let context: ControlContext = serde_json::from_value(json!({"policy":{"dependent_provider":"consumer","owner_provider":"owner","owner_service":"owner/2","owner_method":method,"status_method":method,"settlement_method":method,"argument_schema":schema,"output_capability":"display","output_schema":schema,"max_access":"exclusive","mutation_domains":[]},"output":{"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"data":{}},"target":target,"transition_id":"transition","parent_generation":1})).unwrap();
        let participant: ParticipantSettings = serde_json::from_value(json!({"provider":"consumer","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}})).unwrap();
        let request = json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":resource,"method":method,"argument_schema":schema,"arguments":{"width":1920},"access":"exclusive","mutation_domains":[]},"admission_id":"admission","permit_id":"permit","remaining_ms":5000});
        (
            Backend {
                resolved: ResolvedControl {
                    participant,
                    context,
                    session_target: serde_json::from_value(json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1})).unwrap(),
                    consumers_retired: false,
                    resource_generation: 1,
                },
                schema_available: true,
                consumed: false,
                rights: true,
                consume_calls: 0,
            },
            json!({"operation":"verify_resource_control","request":request}),
        )
    }
    #[test]
    fn verify_consumes_once_and_pre_effect_validation_is_read_only() {
        let (mut backend, mut payload) = fixture();
        payload["operation"] = json!("describe_resource_control");
        assert_eq!(
            control_endpoint("owner", payload.clone(), &mut backend)["status"],
            "described"
        );
        assert_eq!(backend.consume_calls, 0);
        payload["operation"] = json!("verify_resource_control");
        let first = control_endpoint("owner", payload.clone(), &mut backend);
        assert_eq!(first["status"], "execute");
        assert_eq!(first["session_target"]["generation"], 1);
        assert_eq!(first["resource_generation"], 1);
        assert_eq!(first["output"]["output_id"], "out");
        assert_eq!(first["consumers_retired"], false);
        assert_eq!(
            control_endpoint("owner", payload.clone(), &mut backend)["status"],
            "reattach_only"
        );
        payload["operation"] = json!("validate_resource_control");
        assert_eq!(
            control_endpoint("owner", payload.clone(), &mut backend)["status"],
            "valid"
        );
        assert_eq!(backend.consume_calls, 2);
        backend.rights = false;
        assert_eq!(
            control_endpoint("owner", payload, &mut backend)["status"],
            "rejected"
        );
    }
    #[test]
    fn forged_or_unavailable_context_and_schema_never_consume() {
        let (mut backend, payload) = fixture();
        assert_eq!(
            control_endpoint("attacker", payload.clone(), &mut backend)["status"],
            "rejected"
        );
        backend.resolved.context.output.resource.revision += 1;
        assert_eq!(
            control_endpoint("owner", payload.clone(), &mut backend)["status"],
            "rejected"
        );
        backend.resolved.context.output.resource.revision -= 1;
        backend.schema_available = false;
        assert_eq!(
            control_endpoint("owner", payload, &mut backend)["status"],
            "rejected"
        );
        assert_eq!(backend.consume_calls, 0);
    }
    #[test]
    fn validation_without_consumption_and_malformed_requests_fail_closed() {
        let (mut backend, mut payload) = fixture();
        payload["operation"] = json!("validate_resource_control");
        assert_eq!(
            control_endpoint("owner", payload.clone(), &mut backend)["status"],
            "rejected"
        );
        payload["request"]["target"]["generation"] = json!(2);
        assert_eq!(
            control_endpoint("owner", payload, &mut backend)["status"],
            "rejected"
        );
        assert_eq!(backend.consume_calls, 0);
    }
}
