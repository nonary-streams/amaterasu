//! Provider-owned HDR color profile selections.
//!
//! Profile identifiers are opaque catalog IDs. They are never filesystem
//! paths; the selected display provider resolves them against its own catalog.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HdrProfileSelection {
    pub id: String,
}

impl HdrProfileSelection {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_empty()
            || self.id.len() > 256
            || self.id.trim() != self.id
            || self.id.contains(['/', '\\'])
            || self.id.chars().any(char::is_control)
        {
            return Err("invalid_hdr_profile_selection");
        }
        Ok(())
    }
}

/// Immutable result of a provider's read-only profile resolution.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HdrProfileDescriptor {
    pub selection: HdrProfileSelection,
    /// Provider-owned revision for the exact profile content resolved during
    /// preflight. Apply must reject a stale revision before native mutation.
    pub revision: String,
    /// Optional mastering peak read from a supported profile metadata tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_luminance_nits: Option<u32>,
}

impl HdrProfileDescriptor {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.selection.validate()?;
        if self.revision.is_empty()
            || self.revision.len() > 128
            || self.revision.chars().any(char::is_control)
            || self
                .peak_luminance_nits
                .is_some_and(|value| !(1..=100_000).contains(&value))
        {
            return Err("invalid_hdr_profile_descriptor");
        }
        Ok(())
    }
}

/// A read-only resolution request binds the selected profile to the exact
/// display target and policy snapshot the caller intends to admit.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HdrProfileResolutionRequest {
    pub selection: HdrProfileSelection,
    /// Version 2 profile transactions bind the provider receipt to the exact
    /// lifecycle session whose active streams lease the display profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_target: Option<amaterasu_extension_api::LifecycleTarget>,
    pub target_id: String,
    pub target_revision: String,
    pub policy_revision: String,
}

impl HdrProfileResolutionRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.selection.validate()?;
        if !valid_binding_token(&self.target_id, 1024)
            || !valid_binding_token(&self.target_revision, 128)
            || !valid_binding_token(&self.policy_revision, 128)
            || self
                .lifecycle_target
                .as_ref()
                .is_some_and(|target| !valid_session_owner_target(target))
        {
            return Err("invalid_hdr_profile_resolution_request");
        }
        Ok(())
    }
}

/// Provider result bound to the exact target and immutable launch policy.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HdrProfileResolution {
    /// Exact opaque identifier requested by the caller. The provider may
    /// canonicalize the resolved profile's catalog filename separately.
    pub requested_selection: HdrProfileSelection,
    /// Echoes the optional v2 lifecycle owner binding from the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_target: Option<amaterasu_extension_api::LifecycleTarget>,
    pub profile: HdrProfileDescriptor,
    pub target_id: String,
    pub target_revision: String,
    pub policy_revision: String,
}

impl HdrProfileResolution {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.requested_selection.validate()?;
        self.profile.validate()?;
        if !valid_binding_token(&self.target_id, 1024)
            || !valid_binding_token(&self.target_revision, 128)
            || !valid_binding_token(&self.policy_revision, 128)
            || self
                .lifecycle_target
                .as_ref()
                .is_some_and(|target| !valid_session_owner_target(target))
        {
            return Err("invalid_hdr_profile_resolution");
        }
        Ok(())
    }

    pub fn matches(&self, request: &HdrProfileResolutionRequest) -> bool {
        self.lifecycle_target == request.lifecycle_target
            && self.target_id == request.target_id
            && self.target_revision == request.target_revision
            && self.policy_revision == request.policy_revision
            && self.requested_selection == request.selection
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HdrProfileApplyReceipt {
    pub receipt_id: String,
    pub operation_id: String,
    pub owner: String,
    /// Durable owner binding present in version 2 transactions. None is
    /// accepted only so persisted version 1 receipts can be identified and
    /// kept fail-closed by version 2 consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_target: Option<amaterasu_extension_api::LifecycleTarget>,
    pub target_id: String,
    pub target_binding_revision: String,
    pub policy_revision: String,
    pub previous: ColorProfileState,
    pub applied: HdrProfileDescriptor,
}

impl HdrProfileApplyReceipt {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_binding_token(&self.receipt_id, 64)
            || !valid_binding_token(&self.operation_id, 128)
            || !valid_binding_token(&self.owner, 128)
            || !valid_binding_token(&self.target_id, 1024)
            || !valid_binding_token(&self.target_binding_revision, 128)
            || !valid_binding_token(&self.policy_revision, 128)
            || self
                .lifecycle_target
                .as_ref()
                .is_some_and(|target| !valid_session_owner_target(target))
            || matches!(
                self.previous,
                ColorProfileState::Unknown | ColorProfileState::Unspecified
            )
        {
            return Err("invalid_hdr_profile_apply_receipt");
        }
        self.previous.validate()?;
        self.applied.validate()
    }
}

fn valid_session_owner_target(target: &amaterasu_extension_api::LifecycleTarget) -> bool {
    use amaterasu_extension_api::LifecycleTarget;
    let LifecycleTarget::Session {
        host_epoch,
        session_id,
        generation,
    } = target
    else {
        return false;
    };
    *generation > 0
        && [host_epoch, session_id].iter().all(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        })
}

fn valid_binding_token(value: &str, max_len: usize) -> bool {
    !value.is_empty() && value.len() <= max_len && !value.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrProfileRecoveryState {
    Applied,
    Restored,
}

/// Current or requested display profile state. `Unspecified` is used only by
/// requests that leave color state unchanged; snapshots use `SystemDefault`,
/// `Selected`, or `Unknown` so restore never guesses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "state", content = "profile", rename_all = "snake_case")]
pub enum ColorProfileState {
    #[default]
    Unspecified,
    SystemDefault,
    Selected(HdrProfileDescriptor),
    Unknown,
}

impl ColorProfileState {
    pub fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Selected(profile) = self {
            profile.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor(id: &str) -> HdrProfileDescriptor {
        HdrProfileDescriptor {
            selection: HdrProfileSelection { id: id.into() },
            revision: "sha256:0123456789abcdef".into(),
            peak_luminance_nits: Some(1200),
        }
    }

    #[test]
    fn hdr_profile_selection_is_an_opaque_bounded_catalog_id() {
        for id in [
            "",
            "  profile",
            "profile ",
            "../secret.icc",
            "C:\\profile.icc",
        ] {
            assert!(HdrProfileSelection { id: id.into() }.validate().is_err());
        }
        assert!(
            HdrProfileSelection {
                id: "Office HDR (MHC2)".into()
            }
            .validate()
            .is_ok()
        );
        assert!(
            HdrProfileSelection {
                id: "x".repeat(257)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn resolved_profile_is_revision_pinned_and_bounds_optional_peak_metadata() {
        let profile = descriptor("display-hdr");
        assert!(profile.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&profile).unwrap(),
            json!({
                "selection":{"id":"display-hdr"},
                "revision":"sha256:0123456789abcdef",
                "peak_luminance_nits":1200
            })
        );
        let mut stale = profile.clone();
        stale.revision.clear();
        assert_eq!(stale.validate(), Err("invalid_hdr_profile_descriptor"));
        let mut invalid_peak = profile;
        invalid_peak.peak_luminance_nits = Some(0);
        assert_eq!(
            invalid_peak.validate(),
            Err("invalid_hdr_profile_descriptor")
        );
    }

    #[test]
    fn color_profile_state_distinguishes_unchanged_default_and_unknown() {
        let states = [
            ColorProfileState::Unspecified,
            ColorProfileState::SystemDefault,
            ColorProfileState::Selected(descriptor("hdr")),
            ColorProfileState::Unknown,
        ];
        for state in states {
            assert_eq!(
                serde_json::from_value::<ColorProfileState>(serde_json::to_value(&state).unwrap())
                    .unwrap(),
                state
            );
        }
    }
}
