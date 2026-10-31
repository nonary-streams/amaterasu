//! Owner-authenticated finalization permit endpoint adapter. Finalization has a
//! distinct authority and never reuses a control permit.
use amaterasu_extension_api::{
    ResourceFinalizeRequest, ResourceOutput, validate_resource_finalize_request,
};
use amaterasu_module_sdk::ModuleResult;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::adapter::ParticipantSettings;
use crate::prepare_authority::{ControlPermitUse, FinalizeContext};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    operation: Operation,
    request: ResourceFinalizeRequest,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Operation {
    VerifyResourceFinalize,
    ValidateResourceFinalize,
}

/// Constructed from the durable coordinator graph, owner selection and output,
/// independently of every field supplied by the caller.
pub(super) struct ResolvedFinalize {
    pub participant: ParticipantSettings,
    pub context: FinalizeContext,
}

/// Implementations must reconstruct trusted selected policy, authenticated
/// output and confirmed controls, current target and parent, active admission,
/// domain rights, installed owner method and required consumer retirement.
/// Schema validation uses exact installed immutable content. `consume` durably
/// records one use before Execute; `validate` is read-only and requires that
/// separate finalize permit to be consumed, unsettled and still current.
pub(super) trait FinalizeEndpointBackend {
    fn resolve(&mut self, request: &ResourceFinalizeRequest) -> ModuleResult<ResolvedFinalize>;
    fn validate_output(&mut self, output: &ResourceOutput) -> ModuleResult<()>;
    fn consume(
        &mut self,
        owner: &str,
        resolved: &ResolvedFinalize,
        request: &ResourceFinalizeRequest,
    ) -> ModuleResult<ControlPermitUse>;
    fn validate(
        &mut self,
        owner: &str,
        resolved: &ResolvedFinalize,
        request: &ResourceFinalizeRequest,
    ) -> ModuleResult<()>;
}

pub(super) fn finalize_endpoint(
    caller: &str,
    payload: Value,
    backend: &mut impl FinalizeEndpointBackend,
) -> Value {
    match authorize(caller, payload, backend) {
        Ok(status) => json!({"status":status}),
        Err(_) => json!({"status":"rejected","reason":"resource_finalize_not_authorized"}),
    }
}

fn authorize(
    caller: &str,
    payload: Value,
    backend: &mut impl FinalizeEndpointBackend,
) -> ModuleResult<&'static str> {
    let envelope: Envelope =
        serde_json::from_value(payload).map_err(|_| "invalid_finalize_envelope")?;
    let request = &envelope.request;
    validate_resource_finalize_request(request)?;
    if caller.is_empty() || caller != request.output.producer {
        return Err("owner_identity_mismatch".into());
    }
    let resolved = backend.resolve(request)?;
    let context = &resolved.context;
    if caller != context.policy.owner_provider
        || resolved.participant.provider != caller
        || context.output != request.output
        || context.control_results != request.control_results
        || context.target != request.target
        || context.transition_id != request.transition_id
        || context.output.producer != caller
        || context.output.producer_service != context.policy.owner_service
        || context.output.capability != context.policy.output_capability
        || context.output.schema != context.policy.output_schema
    {
        return Err("finalize_context_mismatch".into());
    }
    backend.validate_output(&context.output)?;
    for result in &context.control_results {
        backend.validate_output(&result.output)?;
    }
    match envelope.operation {
        Operation::VerifyResourceFinalize => match backend.consume(caller, &resolved, request)? {
            ControlPermitUse::Execute => Ok("execute"),
            ControlPermitUse::ReattachOnly => Ok("reattach_only"),
        },
        Operation::ValidateResourceFinalize => {
            backend.validate(caller, &resolved, request)?;
            Ok("valid")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Backend {
        resolved: ResolvedFinalize,
        schema: bool,
        consumed: bool,
        rights: bool,
        calls: usize,
    }
    impl FinalizeEndpointBackend for Backend {
        fn resolve(&mut self, _: &ResourceFinalizeRequest) -> ModuleResult<ResolvedFinalize> {
            if !self.rights {
                return Err("stale".into());
            }
            Ok(ResolvedFinalize {
                participant: self.resolved.participant.clone(),
                context: self.resolved.context.clone(),
            })
        }
        fn validate_output(&mut self, _: &ResourceOutput) -> ModuleResult<()> {
            if self.schema {
                Ok(())
            } else {
                Err("schema unavailable".into())
            }
        }
        fn consume(
            &mut self,
            _: &str,
            _: &ResolvedFinalize,
            _: &ResourceFinalizeRequest,
        ) -> ModuleResult<ControlPermitUse> {
            self.calls += 1;
            if self.consumed {
                Ok(ControlPermitUse::ReattachOnly)
            } else {
                self.consumed = true;
                Ok(ControlPermitUse::Execute)
            }
        }
        fn validate(
            &mut self,
            _: &str,
            _: &ResolvedFinalize,
            _: &ResourceFinalizeRequest,
        ) -> ModuleResult<()> {
            if self.consumed {
                Ok(())
            } else {
                Err("not consumed".into())
            }
        }
    }
    fn fixture() -> (Backend, Value) {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"data":{}});
        let method = json!({"service":"owner/2","method":"finalize","interface_major":2,"interface_minor":0,"schema_revision":1});
        let context: FinalizeContext = serde_json::from_value(json!({"policy":{"owner_provider":"owner","owner_service":"owner/2","method":method,"output_capability":"display","output_schema":schema,"capture_provider":"capture","selector_schema":"selector","required_access":"shared","mutation_domains":[]},"output":output,"control_results":[],"target":target,"transition_id":"transition","parent_generation":1})).unwrap();
        let participant: ParticipantSettings = serde_json::from_value(json!({"provider":"owner","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}})).unwrap();
        let request = json!({"version":2,"transition_id":"transition","operation_id":"finalize","provider_sequence":1,"target":target,"output":output,"control_results":[],"admission_id":"admission","permit_id":"permit","remaining_ms":5000});
        (
            Backend {
                resolved: ResolvedFinalize {
                    participant,
                    context,
                },
                schema: true,
                consumed: false,
                rights: true,
                calls: 0,
            },
            json!({"operation":"verify_resource_finalize","request":request}),
        )
    }
    #[test]
    fn separate_finalize_permit_is_one_use_and_revalidated() {
        let (mut backend, mut payload) = fixture();
        assert_eq!(
            finalize_endpoint("owner", payload.clone(), &mut backend)["status"],
            "execute"
        );
        assert_eq!(
            finalize_endpoint("owner", payload.clone(), &mut backend)["status"],
            "reattach_only"
        );
        payload["operation"] = json!("validate_resource_finalize");
        assert_eq!(
            finalize_endpoint("owner", payload.clone(), &mut backend)["status"],
            "valid"
        );
        backend.rights = false;
        assert_eq!(
            finalize_endpoint("owner", payload, &mut backend)["status"],
            "rejected"
        );
    }
    #[test]
    fn forged_owner_output_or_missing_schema_never_consumes() {
        let (mut backend, payload) = fixture();
        assert_eq!(
            finalize_endpoint("other", payload.clone(), &mut backend)["status"],
            "rejected"
        );
        backend.resolved.context.output.resource.revision += 1;
        assert_eq!(
            finalize_endpoint("owner", payload.clone(), &mut backend)["status"],
            "rejected"
        );
        backend.resolved.context.output.resource.revision -= 1;
        backend.schema = false;
        assert_eq!(
            finalize_endpoint("owner", payload, &mut backend)["status"],
            "rejected"
        );
        assert_eq!(backend.calls, 0);
    }
}
