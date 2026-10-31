//! Typed action resource disposition. Serialized references identify resources;
//! they are not authority and never replace authenticated dispatch evidence.
use crate::{LifecycleTarget, ResourceRef};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionResourceDispositionIntent {
    Release,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResourceDispositionPlan {
    pub output_id: String,
    pub resource: ResourceRef,
    pub origin: LifecycleTarget,
    pub intent: ActionResourceDispositionIntent,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionResourceDispositionState {
    Released,
    Retained,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionResourceDispositionResult {
    pub output_id: String,
    pub resource: ResourceRef,
    pub origin: LifecycleTarget,
    pub state: ActionResourceDispositionState,
}
/// Exact completed owner payload. The receipt is opaque recovery data, not proof.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionProviderCompleted {
    pub status: ActionProviderCompletionStatus,
    pub receipt: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_complete: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_disposition: Option<ActionResourceDispositionResult>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionProviderCompletionStatus {
    Succeeded,
    Failed,
    RecoveryRequired,
}
fn domains(resource: &ResourceRef) -> Option<BTreeSet<&str>> {
    let mut domains = BTreeSet::new();
    for domain in &resource.mutation_domains {
        if !crate::valid_opaque_id(domain) || !domains.insert(domain.as_str()) {
            return None;
        }
    }
    if let Some(domain) = &resource.mutation_domain {
        if !crate::valid_opaque_id(domain) {
            return None;
        }
        domains.insert(domain);
    }
    Some(domains)
}
fn valid_resource(resource: &ResourceRef) -> bool {
    crate::valid_identifier(&resource.provider)
        && crate::valid_opaque_id(&resource.resource_id)
        && crate::valid_opaque_id(&resource.incarnation)
        && resource.revision > 0
        && crate::valid_opaque_id(&resource.execution_domain)
        && domains(resource).is_some()
}
fn valid_origin(origin: &LifecycleTarget) -> bool {
    match origin {
        LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } => {
            crate::valid_opaque_id(host_epoch)
                && crate::valid_opaque_id(session_id)
                && *generation > 0
        }
        LifecycleTarget::Stream {
            host_epoch,
            session_id,
            stream_id,
            generation,
        } => {
            crate::valid_opaque_id(host_epoch)
                && crate::valid_opaque_id(session_id)
                && crate::valid_opaque_id(stream_id)
                && *generation > 0
        }
        LifecycleTarget::Validation { .. } => false,
    }
}
/// Structural validation only; coordinator must authenticate plan, owner and target.
pub fn validate_action_disposition_plan(
    plan: &ActionResourceDispositionPlan,
) -> Result<(), String> {
    if !crate::valid_opaque_id(&plan.output_id)
        || !valid_resource(&plan.resource)
        || !valid_origin(&plan.origin)
    {
        return Err("invalid action disposition plan".into());
    }
    Ok(())
}
/// Validate resource identity and monotonic post-effect revision; authorization,
/// callback provenance and closure of dependents are separate coordinator checks.
pub fn validate_action_disposition_result(
    plan: &ActionResourceDispositionPlan,
    result: &ActionResourceDispositionResult,
) -> Result<(), String> {
    validate_action_disposition_plan(plan)?;
    let old = &plan.resource;
    let new = &result.resource;
    if !valid_resource(new)
        || result.output_id != plan.output_id
        || result.origin != plan.origin
        || old.provider != new.provider
        || old.resource_id != new.resource_id
        || old.incarnation != new.incarnation
        || old.execution_domain != new.execution_domain
        || domains(old) != domains(new)
        || new.revision < old.revision
    {
        return Err("action disposition changed selected resource".into());
    }
    Ok(())
}

/// Validate the result shape against the frozen plan; authenticated owner
/// dispatch, target, plan digest, permit and retirement are checked separately.
pub fn validate_action_provider_completed(
    plan: Option<&ActionResourceDispositionPlan>,
    completed: &ActionProviderCompleted,
) -> Result<(), String> {
    if serde_json::to_vec(completed)
        .map_err(|error| error.to_string())?
        .len()
        > 12 * 1024
    {
        return Err("action provider result exceeds size limit".into());
    }
    match (plan, completed.resource_disposition.as_ref()) {
        (Some(plan), Some(result)) => {
            validate_action_disposition_result(plan, result)?;
            if result.state == ActionResourceDispositionState::Released
                && completed.status != ActionProviderCompletionStatus::Succeeded
            {
                return Err("released disposition requires successful completion".into());
            }
        }
        (Some(_), None) if completed.status == ActionProviderCompletionStatus::Succeeded => {
            return Err("successful disposition result missing".into());
        }
        (None, Some(_)) => return Err("unplanned action disposition".into()),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ResourceAccess, ResourcePlan, validate_resource_plan};
    use serde_json::json;

    fn fixtures() -> (ResourcePlan, ActionResourceDispositionResult) {
        let resource = json!({"provider":"owner","resource_id":"display","incarnation":"native","revision":3,"execution_domain":"seat","mutation_domain":"topology","mutation_domains":["topology"]});
        let origin =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":2});
        let plan = serde_json::from_value(json!({"plan_id":"plan","revision":1,"resources":[{"provider":"owner","resource_id":"display","incarnation":"native","expected_revision":3,"execution_domain":"seat","mutation_domains":["topology"],"access":"exclusive"}],"mutation_domains":[{"domain":"topology","expected_revision":1,"borrowers":{},"access":"exclusive"}],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[],"resource_disposition":{"output_id":"screen","resource":resource,"origin":origin,"intent":"release"}})).unwrap();
        let result = serde_json::from_value(
            json!({"output_id":"screen","resource":resource,"origin":origin,"state":"released"}),
        )
        .unwrap();
        (plan, result)
    }
    #[test]
    fn disposition_binds_exact_admitted_resource_and_owner_response() {
        let (mut plan, mut result) = fixtures();
        assert!(validate_resource_plan(&plan).is_ok());
        let disposition = plan.resource_disposition.clone().unwrap();
        assert!(validate_action_disposition_result(&disposition, &result).is_ok());
        result.resource.revision += 1;
        assert!(validate_action_disposition_result(&disposition, &result).is_ok());
        result.resource.incarnation = "different".into();
        assert!(validate_action_disposition_result(&disposition, &result).is_err());
        result = fixtures().1;
        result.origin = LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 3,
        };
        assert!(validate_action_disposition_result(&disposition, &result).is_err());
        result = fixtures().1;
        result.resource.revision -= 1;
        assert!(validate_action_disposition_result(&disposition, &result).is_err());
        plan.resources[0].access = ResourceAccess::Read;
        assert!(validate_resource_plan(&plan).is_err());
        let completed: ActionProviderCompleted = serde_json::from_value(
            json!({"status":"succeeded","receipt":{},"resource_disposition":fixtures().1}),
        )
        .unwrap();
        assert!(validate_action_provider_completed(Some(&disposition), &completed).is_ok());
        let mut failed = completed.clone();
        failed.status = ActionProviderCompletionStatus::Failed;
        assert!(validate_action_provider_completed(Some(&disposition), &failed).is_err());
        assert!(validate_action_provider_completed(None, &completed).is_err());
        assert_eq!(
            serde_json::to_value(completed).unwrap()["resource_disposition"]["state"],
            "released"
        );
    }
    #[test]
    fn legacy_plan_omits_disposition_and_unknown_fields_are_rejected() {
        let (mut plan, _) = fixtures();
        plan.resource_disposition = None;
        let serialized = serde_json::to_value(&plan).unwrap();
        assert!(serialized.get("resource_disposition").is_none());
        assert_eq!(
            serde_json::from_value::<ResourcePlan>(serialized).unwrap(),
            plan
        );
        assert!(
            serde_json::from_value::<ActionProviderCompleted>(
                json!({"status":"succeeded","receipt":{},"authority":"forged"})
            )
            .is_err()
        );
    }
}
