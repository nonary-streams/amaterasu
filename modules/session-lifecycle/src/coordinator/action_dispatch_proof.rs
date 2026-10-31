//! Structural verification of action-service-attested broker dispatch evidence.
//! This does not establish current rights, cleanup, output provenance, or release.
use crate::action_authority::{ActionApprovalSnapshot, ActionBinding};
use amaterasu_extension_api::{ActionProviderCompleted, MethodRef, ResourcePlan};
use amaterasu_module_sdk::Command;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedCall {
    approval: Value,
    call: Command,
    result: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchProof {
    effect: ApprovedCall,
    status: Vec<ApprovedCall>,
}

fn call_payload<'a>(
    call: &'a ApprovedCall,
    approval: &Value,
    method: &MethodRef,
) -> Option<&'a Value> {
    if &call.approval != approval {
        return None;
    }
    let Command::CallApi {
        service,
        method: name,
        interface_major,
        interface_minor,
        payload,
        timeout_ms,
        operation_id,
        ..
    } = &call.call
    else {
        return None;
    };
    (service == &method.service
        && name == &method.method
        && *interface_major == method.interface_major
        && *interface_minor == method.interface_minor
        && *timeout_ms > 0
        && *timeout_ms <= 30_000
        && !operation_id.is_empty()
        && operation_id.len() <= 128)
        .then_some(payload)
}

fn payload_matches(payload: &Value, binding: &ActionBinding) -> bool {
    payload["version"] == 1
        && payload["operation_id"].as_str() == Some(&binding.operation_id)
        && payload["action_id"].as_str() == Some(&binding.action_id)
        && serde_json::to_value(&binding.target).ok().as_ref() == payload.get("target")
        && payload["plan_digest"].as_str() == Some(&binding.plan_digest)
}

/// Check the exact frozen dispatch, pending chain, and typed final owner response.
/// The caller must separately authenticate the action service and validate current
/// target, graph cleanup proof, active rights, and output generation under lock.
pub(super) fn validate_dispatch_proof(
    raw: &Value,
    completed: &Value,
    receipt: &Value,
    binding: &ActionBinding,
    permit: &str,
    plan: &ResourcePlan,
    approvals: &ActionApprovalSnapshot,
) -> Result<ActionProviderCompleted, &'static str> {
    if serde_json::to_vec(raw).map_or(true, |v| v.len() > 64 * 1024) {
        return Err("action_dispatch_proof_invalid");
    }
    let proof: DispatchProof =
        serde_json::from_value(raw.clone()).map_err(|_| "action_dispatch_proof_invalid")?;
    if proof.status.len() > 8 {
        return Err("action_dispatch_proof_invalid");
    }
    let execute_approval = serde_json::to_value(&approvals.execute_approval)
        .map_err(|_| "action_dispatch_proof_invalid")?;
    let effect = call_payload(
        &proof.effect,
        &execute_approval,
        &approvals.descriptor.execute,
    )
    .ok_or("action_dispatch_proof_invalid")?;
    if !payload_matches(effect, binding)
        || effect["permit"].as_str() != Some(permit)
        || serde_json::to_value(plan).ok().as_ref() != effect.get("plan")
    {
        return Err("action_dispatch_proof_invalid");
    }
    let mut result = proof
        .effect
        .result
        .as_ref()
        .ok_or("action_dispatch_proof_invalid")?;
    for step in &proof.status {
        if result["status"].as_str() != Some("pending") {
            return Err("action_dispatch_proof_invalid");
        }
        let prior_receipt = result
            .get("receipt")
            .ok_or("action_dispatch_proof_invalid")?;
        let status_method = approvals
            .descriptor
            .status
            .as_ref()
            .ok_or("action_dispatch_proof_invalid")?;
        let status_approval = serde_json::to_value(
            approvals
                .status_approval
                .as_ref()
                .ok_or("action_dispatch_proof_invalid")?,
        )
        .map_err(|_| "action_dispatch_proof_invalid")?;
        let payload = call_payload(step, &status_approval, status_method)
            .ok_or("action_dispatch_proof_invalid")?;
        if !payload_matches(payload, binding)
            || payload.get("provider_receipt") != Some(prior_receipt)
        {
            return Err("action_dispatch_proof_invalid");
        }
        result = step
            .result
            .as_ref()
            .ok_or("action_dispatch_proof_invalid")?;
    }
    if result != completed || result.get("receipt") != Some(receipt) {
        return Err("action_dispatch_proof_invalid");
    }
    let completed: ActionProviderCompleted =
        serde_json::from_value(completed.clone()).map_err(|_| "action_dispatch_proof_invalid")?;
    amaterasu_extension_api::validate_action_provider_completed(
        plan.resource_disposition.as_deref(),
        &completed,
    )
    .map_err(|_| "action_dispatch_proof_invalid")?;
    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dispatch_chain_rejects_drift_and_unbound_pending_receipts() {
        let execute = json!({"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        let status = json!({"service":"display/1","method":"status","interface_major":1,"interface_minor":0,"schema_revision":1});
        let api_method = |name: &str| json!({"provider":"display","callers":["action-service"],"method":{"service":"display/1","method":name,"interface_major":1,"interface_minor":0,"request":{"required":{},"optional":{},"allow_extra":false},"response":{"required":{},"optional":{},"allow_extra":false},"privilege":"user"}});
        let approvals: ActionApprovalSnapshot = serde_json::from_value(json!({"descriptor":{"id":"display/remove","label":"Remove","description":"Remove","execution":"session_mutation","arguments":{},"allowed_targets":["stream"],"required_capabilities":[],"plan":null,"execute":execute,"status":status},"execute_approval":api_method("execute"),"status_approval":api_method("status")})).unwrap();
        let binding: ActionBinding = serde_json::from_value(json!({"operation_id":"op","action_id":"display/remove","target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","expected_generation":2},"plan_digest":"a".repeat(64),"source":{"caller":"transport","initiator":"user"}})).unwrap();
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let identity = json!({"version":1,"operation_id":"op","action_id":"display/remove","target":binding.target,"plan_digest":binding.plan_digest});
        let mut effect = identity.clone();
        effect["permit"] = json!("token");
        effect["plan"] = serde_json::to_value(&plan).unwrap();
        let mut status_payload = identity;
        status_payload["provider_receipt"] = json!({"pending":1});
        let call = |name: &str, payload: Value| json!({"op":"call_api","service":"display/1","method":name,"interface_major":1,"interface_minor":0,"payload":payload,"timeout_ms":5000,"operation_id":"dispatch","parent":null});
        let completed = json!({"status":"succeeded","receipt":{"done":true}});
        let proof = json!({"effect":{"approval":api_method("execute"),"call":call("execute", effect),"result":{"status":"pending","receipt":{"pending":1}}},"status":[{"approval":api_method("status"),"call":call("status",status_payload),"result":completed}]});
        let check = |p: &Value| {
            validate_dispatch_proof(
                p,
                &completed,
                &completed["receipt"],
                &binding,
                "token",
                &plan,
                &approvals,
            )
            .is_ok()
        };
        assert!(check(&proof));
        let mut scalar_receipt = proof.clone();
        scalar_receipt["effect"]["result"]["receipt"] = json!("pending-token");
        scalar_receipt["status"][0]["call"]["payload"]["provider_receipt"] = json!("pending-token");
        assert!(check(&scalar_receipt));
        for path in ["provider", "schema", "receipt", "target"] {
            let mut altered = proof.clone();
            match path {
                "provider" => altered["effect"]["approval"]["provider"] = json!("foreign"),
                "schema" => {
                    altered["effect"]["approval"]["method"]["request"]["allow_extra"] = json!(true)
                }
                "receipt" => {
                    altered["status"][0]["call"]["payload"]["provider_receipt"] =
                        json!({"pending":2})
                }
                _ => {
                    altered["effect"]["call"]["payload"]["target"]["expected_generation"] = json!(3)
                }
            }
            assert!(!check(&altered), "accepted changed {path}");
        }
    }
}
