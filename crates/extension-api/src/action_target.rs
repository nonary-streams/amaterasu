//! Versioned planning context supplied by the lifecycle coordinator.
//! These records describe selected resources; they do not authorize effects.
use crate::{
    ActionTarget, CaptureBinding, LifecyclePhase, LifecycleTarget, ResourceObligation,
    ResourceOutput, ResourcePlan,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A v2 response to a provider-authenticated target-description request.
/// The v1 response is unchanged and is not represented by this versioned type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTargetDescription {
    pub version: u32,
    pub status: ActionTargetDescriptionStatus,
    pub provider: String,
    pub target: ActionTarget,
    pub session_target: LifecycleTarget,
    pub execution_domain: String,
    pub settings_revision: String,
    pub settings: Value,
    /// Canonical retained owner output, when available, independently verified by the coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_output: Option<ResourceOutput>,
    pub streams: Vec<ActionTargetStream>,
    pub obligations: Vec<ResourceObligation>,
    pub mutation_domains: BTreeMap<String, ActionTargetDomain>,
    pub admitted_plans: Vec<ActionTargetAdmittedPlan>,
    pub resource_targets: Vec<LifecycleTarget>,
    pub stale_mutation_domains: Vec<String>,
}

/// Check v2 output structure; caller identity, provenance and freshness require separate verification.
pub fn validate_action_target_description(
    description: &ActionTargetDescription,
) -> Result<(), String> {
    if description.version != crate::COMPOSITION_VERSION
        || serde_json::to_vec(description).map_or(true, |bytes| bytes.len() > 16 * 1024)
    {
        return Err("invalid action target description".into());
    }
    if let Some(output) = &description.owner_output {
        crate::validate_resource_output(output)?;
        if output.producer != description.provider
            || !match (&output.origin, &description.session_target) {
                (
                    LifecycleTarget::Stream {
                        host_epoch,
                        session_id,
                        ..
                    },
                    LifecycleTarget::Session {
                        host_epoch: expected_epoch,
                        session_id: expected_id,
                        ..
                    },
                ) => host_epoch == expected_epoch && session_id == expected_id,
                _ => crate::output_in_scope(output, &description.session_target),
            }
            || output.resource.execution_domain != description.execution_domain
        {
            return Err("owner output outside described session".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTargetDescriptionStatus {
    Succeeded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTargetStream {
    pub target: ActionTarget,
    pub lifecycle_target: LifecycleTarget,
    pub phase: ActionTargetStreamPhase,
    pub candidate: Option<CaptureBinding>,
    pub consumers_retired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionTargetStreamPhase {
    Preparing,
    Ready,
    Active,
    Quiescing,
    Retired,
    RecoveryRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTargetDomain {
    pub revision: u64,
    pub borrowers: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionTargetAdmittedPlan {
    pub admission_id: String,
    pub target: LifecycleTarget,
    pub phase: LifecyclePhase,
    pub plan: ResourcePlan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn v2_description_round_trips_paused_session_without_streams() {
        let target = json!({"kind":"session","host_epoch":"epoch","session_id":"session","expected_generation":3});
        let session_target =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":3});
        let value = json!({"version":2,"status":"succeeded","provider":"example.display","target":target,"session_target":session_target,"execution_domain":"seat","settings_revision":"snapshot","settings":{},"streams":[],"obligations":[],"mutation_domains":{"domain":{"revision":2,"borrowers":{}}},"admitted_plans":[],"resource_targets":[],"stale_mutation_domains":[]});
        let decoded: ActionTargetDescription = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
        let mut unknown = value;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<ActionTargetDescription>(unknown).is_err());
    }
    #[test]
    fn owner_output_must_match_provider_and_session_scope() {
        let value = json!({"version":2,"status":"succeeded","provider":"example.display","target":{"kind":"session","host_epoch":"epoch","session_id":"session","expected_generation":3},"session_target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":3},"execution_domain":"seat","settings_revision":"snapshot","settings":{},"streams":[],"obligations":[],"mutation_domains":{},"admitted_plans":[],"resource_targets":[],"stale_mutation_domains":[],"owner_output":{"output_id":"output","capability":"example.display","schema":{"id":"example.schema","major":2,"minor":0,"digest":"a".repeat(64)},"producer":"example.display","producer_service":"example.display/2","origin":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":3},"resource":{"provider":"example.display","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}}});
        let description: ActionTargetDescription = serde_json::from_value(value).unwrap();
        assert!(validate_action_target_description(&description).is_ok());
        let mut foreign = description;
        foreign.owner_output.as_mut().unwrap().producer = "foreign".into();
        assert!(validate_action_target_description(&foreign).is_err());
    }
}
