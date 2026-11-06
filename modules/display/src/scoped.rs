//! Durable reservations for independently owned outputs sharing one desktop.
use crate::model::{
    Configuration, DisplayError, DisplayResult, DisplaySetting, Journal, RecoveryBaseline,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_SCOPED_TARGETS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedReservation {
    pub owner: String,
    pub identity: String,
    pub reference_id: String,
    pub target_id: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_identity: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedPending {
    pub owner: String,
    pub operation_id: String,
    pub request_identity: Value,
    pub target_id: String,
    pub before_configuration: Configuration,
    pub desired_configuration: Configuration,
    pub releasing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<crate::model::RouteBinding>,
}

pub fn validate_identity(value: &str) -> DisplayResult<()> {
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(DisplayError::new(
            "invalid_request",
            "invalid scoped identity",
        ));
    }
    Ok(())
}

pub fn validate_journal(journal: &Journal) -> DisplayResult<()> {
    if journal.scoped_targets.len() > MAX_SCOPED_TARGETS {
        return Err(DisplayError::new(
            "invalid_journal",
            "too many protected targets",
        ));
    }
    let mut targets = BTreeSet::new();
    let mut references = BTreeSet::new();
    for target in &journal.scoped_targets {
        validate_identity(&target.owner)?;
        validate_identity(&target.identity)?;
        validate_identity(&target.reference_id)?;
        validate_identity(&target.operation_id)?;
        if target.target_id.is_empty()
            || target.target_id.len() > 4096
            || target.target_id.chars().any(char::is_control)
            || !targets.insert(target.target_id.to_lowercase())
            || !references.insert((&target.owner, &target.identity, &target.reference_id))
        {
            return Err(DisplayError::new(
                "invalid_journal",
                "ambiguous protected target",
            ));
        }
    }
    if let Some(pending) = &journal.scoped_pending {
        validate_identity(&pending.owner)?;
        validate_identity(&pending.operation_id)?;
        pending.before_configuration.validate()?;
        pending.desired_configuration.validate()?;
        if !journal
            .scoped_targets
            .iter()
            .any(|target| target.owner == pending.owner && target.target_id == pending.target_id)
        {
            return Err(DisplayError::new(
                "invalid_journal",
                "pending target has no protected owner",
            ));
        }
        let operation =
            serde_json::from_value::<crate::model::Operation>(pending.request_identity.clone())
                .map_err(|_| DisplayError::new("invalid_journal", "invalid scoped operation"))?;
        let consistent = match operation {
            crate::model::Operation::ScopedApply { operation_id, .. } => {
                !pending.releasing && operation_id == pending.operation_id
            }
            crate::model::Operation::ScopedRelease { operation_id, .. } => {
                pending.releasing && operation_id == pending.operation_id
            }
            _ => false,
        };
        if !consistent {
            return Err(DisplayError::new(
                "invalid_journal",
                "scoped operation changed",
            ));
        }
    }
    Ok(())
}

pub fn same_setting(expected: &DisplaySetting, observed: &DisplaySetting) -> bool {
    expected.id.eq_ignore_ascii_case(&observed.id)
        && expected.width == observed.width
        && expected.height == observed.height
        && u64::from(expected.refresh.numerator) * u64::from(observed.refresh.denominator)
            == u64::from(observed.refresh.numerator) * u64::from(expected.refresh.denominator)
        && expected.x == observed.x
        && expected.y == observed.y
        && std::mem::discriminant(&expected.rotation) == std::mem::discriminant(&observed.rotation)
        && expected
            .hdr_enabled
            .is_none_or(|enabled| observed.hdr_enabled == Some(enabled))
}

pub fn same_configuration(expected: &Configuration, observed: &Configuration) -> bool {
    expected.displays.len() == observed.displays.len()
        && expected.displays.iter().all(|display| {
            observed
                .displays
                .iter()
                .any(|current| same_setting(display, current))
        })
}

/// Copy only the protected output's current native path into a normal lease's
/// recovery baseline. The original owner still restores its original targets;
/// a later monitor or its changed mode remains independently owned.
pub fn merge_current_target(
    baseline: &mut RecoveryBaseline,
    current: &RecoveryBaseline,
    target_id: &str,
) -> DisplayResult<()> {
    match (baseline, current) {
        (RecoveryBaseline::Ccd(saved), RecoveryBaseline::Ccd(observed)) => {
            let target = observed
                .paths
                .iter()
                .find(|path| path.id.eq_ignore_ascii_case(target_id))
                .ok_or_else(|| {
                    DisplayError::new(
                        "protected_target_unavailable",
                        "protected output is no longer observable",
                    )
                })?;
            saved
                .paths
                .retain(|path| !path.id.eq_ignore_ascii_case(target_id));
            saved.paths.push(target.clone());
            saved.validate()
        }
        (RecoveryBaseline::KScreen(saved), RecoveryBaseline::KScreen(observed)) => {
            let target = observed
                .outputs
                .iter()
                .find(|output| {
                    output.id.eq_ignore_ascii_case(target_id) && output.connected && output.enabled
                })
                .ok_or_else(|| {
                    DisplayError::new(
                        "protected_target_unavailable",
                        "protected output is no longer active",
                    )
                })?;
            saved
                .outputs
                .retain(|output| !output.id.eq_ignore_ascii_case(target_id));
            saved.outputs.push(target.clone());
            saved.validate()
        }
        _ => Err(DisplayError::new(
            "unsupported_snapshot",
            "display recovery provider changed",
        )),
    }
}

pub fn forget_target(baseline: &mut RecoveryBaseline, target_id: &str) -> DisplayResult<()> {
    match baseline {
        RecoveryBaseline::Ccd(saved) => saved
            .paths
            .retain(|path| !path.id.eq_ignore_ascii_case(target_id)),
        RecoveryBaseline::KScreen(saved) => saved
            .outputs
            .retain(|output| !output.id.eq_ignore_ascii_case(target_id)),
    }
    baseline.validate()
}
