//! Public, versioned protocol for separately executed driver installers.
//! Declarations request installation; they never grant administrator authority.
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const MAX_REQUIREMENTS: usize = 32;
pub const MAX_HELPER_REPLY_BYTES: usize = 64 * 1024;
/// A separately provisioned provider owns privileged installation and host barriers.
pub const INSTALLATION_SERVICE: &str = "nonary.plugin-installation/1";

/// The same finite management contract is available to replacement providers.
pub fn installation_methods() -> Vec<crate::ApiMethod> {
    use crate::{ApiFieldKind as Kind, operation_api::method};
    vec![
        method(INSTALLATION_SERVICE, "status", &[], &[]),
        method(
            INSTALLATION_SERVICE,
            "prepare",
            &[
                ("repository", Kind::String),
                ("manifest", Kind::String),
                ("installation_revision", Kind::String),
            ],
            &[],
        ),
        method(
            INSTALLATION_SERVICE,
            "approve",
            &[
                ("operation", Kind::String),
                ("revision", Kind::String),
                ("installation_revision", Kind::String),
                ("approval", Kind::String),
            ],
            &[],
        ),
        method(
            INSTALLATION_SERVICE,
            "cancel",
            &[
                ("operation", Kind::String),
                ("revision", Kind::String),
                ("installation_revision", Kind::String),
            ],
            &[],
        ),
        method(
            INSTALLATION_SERVICE,
            "retry",
            &[
                ("operation", Kind::String),
                ("revision", Kind::String),
                ("installation_revision", Kind::String),
            ],
            &[],
        ),
        method(
            INSTALLATION_SERVICE,
            "activate",
            &[
                ("operation", Kind::String),
                ("revision", Kind::String),
                ("installation_revision", Kind::String),
            ],
            &[],
        ),
    ]
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverRequirement {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub target: String,
    /// Stable feature identity, e.g. `controller.xbox`; not an executable predicate.
    pub feature: String,
    pub reason: String,
    /// Provider-owned compatibility contract, checked by the selected helper.
    pub compatibility: String,
    pub may_require_reboot: bool,
    /// Version bundled with these signed artifacts, not a claim about upstream latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_version: Option<OfferedVersion>,
    pub installer: Artifact,
    pub payloads: Vec<Artifact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Detect,
    Install,
    Reconcile,
    Verify,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub schema: u32,
    pub operation: String,
    pub action: Action,
    pub requirement: DriverRequirement,
    /// The account under which usability must be checked, not the broker account.
    pub runtime_identity: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverState {
    Checking,
    Missing,
    Incompatible,
    AccessDenied,
    Unsupported,
    Installing,
    Ready,
    RebootRequired,
    ManualActionRequired,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperReply {
    pub schema: u32,
    pub operation: String,
    pub driver: String,
    pub state: DriverState,
    pub installed_version: Option<String>,
    pub message: String,
    /// Whether cancellation can stop work before the next machine change.
    pub cancellable: bool,
}

impl HelperReply {
    pub fn validate_for(&self, request: &HelperRequest) -> Result<(), String> {
        if self.schema != SCHEMA
            || request.schema != SCHEMA
            || self.operation != request.operation
            || self.driver != request.requirement.id
            || self.message.len() > 4096
            || self.message.chars().any(char::is_control)
            || self
                .installed_version
                .as_ref()
                .is_some_and(|value| value.len() > 128 || value.chars().any(char::is_control))
            || serde_json::to_vec(self)
                .map_err(|error| error.to_string())?
                .len()
                > MAX_HELPER_REPLY_BYTES
        {
            return Err("driver_helper_invalid_reply".into());
        }
        if self.state == DriverState::Installing
            && !matches!(request.action, Action::Install | Action::Reconcile)
        {
            return Err("driver_helper_invalid_transition".into());
        }
        Ok(())
    }
}

/// Explicit publisher opt-in to a version ordering. Other formats stay unknown.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfferedVersion {
    pub version: String,
    pub scheme: VersionScheme,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionScheme {
    /// One to eight decimal components; trailing zero components are equivalent.
    Numeric,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverUpdate {
    Unknown,
    Available,
    Current,
    NewerInstalled,
}
fn numeric_version(value: &str) -> Option<Vec<u64>> {
    if value.is_empty() || value.len() > 128 {
        return None;
    }
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() > 8 {
        return None;
    }
    parts
        .into_iter()
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        })
        .collect()
}
impl OfferedVersion {
    pub fn validate(&self) -> bool {
        match self.scheme {
            VersionScheme::Numeric => numeric_version(&self.version).is_some(),
        }
    }
    pub fn compare(&self, installed: Option<&str>) -> DriverUpdate {
        let Some(mut offered) = numeric_version(&self.version) else {
            return DriverUpdate::Unknown;
        };
        let Some(mut installed) = installed.and_then(numeric_version) else {
            return DriverUpdate::Unknown;
        };
        let length = offered.len().max(installed.len());
        offered.resize(length, 0);
        installed.resize(length, 0);
        match installed.cmp(&offered) {
            std::cmp::Ordering::Less => DriverUpdate::Available,
            std::cmp::Ordering::Equal => DriverUpdate::Current,
            std::cmp::Ordering::Greater => DriverUpdate::NewerInstalled,
        }
    }
}
#[cfg(test)]
mod version_tests {
    use super::*;
    #[test]
    fn numeric_versions_compare_components_without_guessing_other_formats() {
        let offered = OfferedVersion {
            version: "1.10.0".into(),
            scheme: VersionScheme::Numeric,
        };
        for (installed, expected) in [
            (Some("1.9"), DriverUpdate::Available),
            (Some("1.10"), DriverUpdate::Current),
            (Some("1.11.0.1"), DriverUpdate::NewerInstalled),
            (None, DriverUpdate::Unknown),
            (Some("1.10-beta"), DriverUpdate::Unknown),
            (Some("18446744073709551616"), DriverUpdate::Unknown),
            (Some("1..2"), DriverUpdate::Unknown),
        ] {
            assert_eq!(offered.compare(installed), expected);
        }
        assert!(
            !OfferedVersion {
                version: "kernel".into(),
                scheme: VersionScheme::Numeric
            }
            .validate()
        );
    }
}
