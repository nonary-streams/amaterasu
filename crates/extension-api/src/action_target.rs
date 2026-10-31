//! Versioned planning context supplied by the lifecycle coordinator.
//! These records describe selected resources; they do not authorize effects.
use crate::{
    ActionTarget, CaptureBinding, LifecyclePhase, LifecycleTarget, ResourceObligation, ResourcePlan,
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
    pub streams: Vec<ActionTargetStream>,
    pub obligations: Vec<ResourceObligation>,
    pub mutation_domains: BTreeMap<String, ActionTargetDomain>,
    pub admitted_plans: Vec<ActionTargetAdmittedPlan>,
    pub resource_targets: Vec<LifecycleTarget>,
    pub stale_mutation_domains: Vec<String>,
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
}
