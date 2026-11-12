//! Version 2 selected display-profile provider operations.
//!
//! The lifecycle target binds a persisted profile receipt to the durable
//! session membership that owns the display while any joined stream remains.
use crate::ModuleResult;
use amaterasu_contracts::display_color_profile::{
    HdrProfileApplyReceipt, HdrProfileResolution, HdrProfileResolutionRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Selected native provider service identity for lifecycle-bound profile calls.
pub const SERVICE: &str = "amaterasu.display-profile/2";
/// Typed operation service identity offered by compatible provider packages.
pub const API_SERVICE: &str = "nonary.display-profile/2";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    ResolveHdrProfile {
        request: HdrProfileResolutionRequest,
    },
    RecoverHdrProfile {},
    PrepareHdrProfile {
        operation_id: String,
        resolution: HdrProfileResolution,
    },
    ApplyHdrProfile {
        receipt: HdrProfileApplyReceipt,
    },
    RestoreHdrProfile {
        receipt: HdrProfileApplyReceipt,
    },
}

impl Request {
    pub fn into_payload(self) -> ModuleResult<Value> {
        serde_json::to_value(self).map_err(|error| error.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryReply {
    pub ok: bool,
    pub found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<HdrProfileApplyReceipt>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_api::LifecycleTarget;
    use amaterasu_contracts::display_color_profile::HdrProfileSelection;

    #[test]
    fn selected_request_carries_exact_session_owner_binding() {
        let target = LifecycleTarget::Session {
            host_epoch: "host-1".into(),
            session_id: "session-1".into(),
            generation: 9,
        };
        let payload = Request::ResolveHdrProfile {
            request: HdrProfileResolutionRequest {
                selection: HdrProfileSelection {
                    id: "studio-hdr".into(),
                },
                lifecycle_target: Some(target.clone()),
                target_id: "display-1".into(),
                target_revision: "topology-1".into(),
                policy_revision: "policy-1".into(),
            },
        }
        .into_payload()
        .unwrap();
        assert_eq!(payload["action"], "resolve_hdr_profile");
        assert_eq!(
            payload["request"]["lifecycle_target"],
            serde_json::to_value(target).unwrap()
        );
        assert_eq!(SERVICE, "amaterasu.display-profile/2");
        assert_eq!(API_SERVICE, "nonary.display-profile/2");
    }
}
