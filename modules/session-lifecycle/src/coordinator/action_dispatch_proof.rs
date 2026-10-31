//! Commitments to broker dispatches attested by the trusted action service.
//! These checks do not establish cleanup, current rights, or resource provenance.
use crate::action_authority::{ActionApprovalSnapshot, ActionBinding};
use amaterasu_extension_api::{ActionProviderCompleted, ResourcePlan};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchCommitment {
    call_digest: String,
    input_receipt_digest: Option<String>,
    result_digest: String,
    pending_receipt_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchProof {
    effect: DispatchCommitment,
    status: Vec<DispatchCommitment>,
    completed: Value,
}

fn digest(value: &Value) -> Option<String> {
    Some(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).ok()?)
    ))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The service attests to exact broker calls and results. Their hashes are
/// commitments, not signatures; only the effect payload and final response can
/// be independently reconstructed here. Caller must separately authenticate
/// the service and check current target, graph cleanup, rights, and generation.
pub(super) fn validate_dispatch_proof(
    raw: &Value,
    binding: &ActionBinding,
    permit: &str,
    plan: &ResourcePlan,
    approvals: &ActionApprovalSnapshot,
) -> Result<ActionProviderCompleted, &'static str> {
    if serde_json::to_vec(raw).map_or(true, |v| v.len() > 16 * 1024) {
        return Err("action_dispatch_proof_invalid");
    }
    let proof: DispatchProof =
        serde_json::from_value(raw.clone()).map_err(|_| "action_dispatch_proof_invalid")?;
    let invoke = approvals
        .invoke
        .as_ref()
        .ok_or("action_dispatch_proof_invalid")?;
    if invoke.operation_id != binding.operation_id
        || invoke.action_id != binding.action_id
        || serde_json::to_value(&invoke.target).ok() != serde_json::to_value(&binding.target).ok()
        || approvals.descriptor.id != binding.action_id
        || proof.status.len() > 8
        || (!proof.status.is_empty()
            && (approvals.descriptor.status.is_none() || approvals.status_approval.is_none()))
    {
        return Err("action_dispatch_proof_invalid");
    }
    let effect_payload = json!({"version":1,"operation_id":binding.operation_id,"action_id":binding.action_id,"target":binding.target,"arguments":invoke.arguments,"expected_settings_revision":invoke.expected_settings_revision,"plan":plan,"plan_digest":binding.plan_digest,"permit":permit});
    if digest(&effect_payload).as_deref() != Some(proof.effect.call_digest.as_str())
        || proof.effect.input_receipt_digest.is_some()
    {
        return Err("action_dispatch_proof_invalid");
    }
    let mut previous = &proof.effect;
    for step in &proof.status {
        if !valid_digest(&previous.result_digest)
            || !valid_digest(&step.call_digest)
            || previous
                .pending_receipt_digest
                .as_ref()
                .is_none_or(|digest| !valid_digest(digest))
            || step.input_receipt_digest != previous.pending_receipt_digest
        {
            return Err("action_dispatch_proof_invalid");
        }
        previous = step;
    }
    if previous.pending_receipt_digest.is_some()
        || digest(&proof.completed).as_deref() != Some(previous.result_digest.as_str())
    {
        return Err("action_dispatch_proof_invalid");
    }
    let completed: ActionProviderCompleted =
        serde_json::from_value(proof.completed).map_err(|_| "action_dispatch_proof_invalid")?;
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

    #[test]
    fn compact_commitment_binds_effect_terminal_and_receipt_chain() {
        let method = json!({"service":"display/1","method":"execute","interface_major":1,"interface_minor":0,"schema_revision":1});
        let status = json!({"service":"display/1","method":"status","interface_major":1,"interface_minor":0,"schema_revision":1});
        let approval = |name: &str| json!({"provider":"display","callers":["action-service"],"method":{"service":"display/1","method":name,"interface_major":1,"interface_minor":0,"request":{"required":{}},"response":{"required":{}},"privilege":"user"}});
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","expected_generation":2});
        let binding: ActionBinding = serde_json::from_value(json!({"operation_id":"op","action_id":"display/remove","target":target,"plan_digest":"a".repeat(64),"source":{"caller":"transport","initiator":"user"}})).unwrap();
        let approvals: ActionApprovalSnapshot = serde_json::from_value(json!({"invoke":{"epoch":"epoch","sequence":1,"operation_id":"op","action_id":"display/remove","target":target,"arguments":{},"expected_settings_revision":null,"input_reservation_id":null},"descriptor":{"id":"display/remove","label":"Remove","description":"Remove","execution":"session_mutation","arguments":{},"allowed_targets":["stream"],"required_capabilities":[],"plan":null,"execute":method,"status":status},"execute_approval":approval("execute"),"status_approval":approval("status")})).unwrap();
        let plan: ResourcePlan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]})).unwrap();
        let payload = json!({"version":1,"operation_id":"op","action_id":"display/remove","target":target,"arguments":{},"expected_settings_revision":null,"plan":plan,"plan_digest":"a".repeat(64),"permit":"token"});
        let terminal = json!({"status":"succeeded","receipt":{"done":true}});
        let receipt_digest = digest(&json!("pending")).unwrap();
        let proof = json!({"effect":{"call_digest":digest(&payload).unwrap(),"input_receipt_digest":null,"result_digest":"b".repeat(64),"pending_receipt_digest":receipt_digest},"status":[{"call_digest":"c".repeat(64),"input_receipt_digest":receipt_digest,"result_digest":digest(&terminal).unwrap(),"pending_receipt_digest":null}],"completed":terminal});
        let check =
            |p: &Value| validate_dispatch_proof(p, &binding, "token", &plan, &approvals).is_ok();
        assert!(check(&proof));
        for field in ["call", "receipt", "terminal"] {
            let mut changed = proof.clone();
            match field {
                "call" => changed["effect"]["call_digest"] = json!("d".repeat(64)),
                "receipt" => changed["status"][0]["input_receipt_digest"] = json!("d".repeat(64)),
                _ => changed["completed"]["receipt"] = json!({"done":false}),
            }
            assert!(!check(&changed), "accepted changed {field}");
        }
        assert!(validate_dispatch_proof(&proof, &binding, "wrong", &plan, &approvals).is_err());
    }
}
