//! Resolved virtual-display intent shared by trusted launch and stream owners.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayConfigurationOption {
    Disabled,
    VerifyOnly,
    EnsureActive,
    EnsurePrimary,
    EnsureOnlyDisplay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayResolutionOption {
    Auto,
    Disabled,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayRefreshRateOption {
    Auto,
    Disabled,
    Manual,
    PreferHighest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VirtualDisplayHdrOption {
    Auto,
    Disabled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VirtualDisplayModeRemapping {
    pub mixed: Vec<VirtualDisplayModeRemappingEntry>,
    pub resolution_only: Vec<VirtualDisplayModeRemappingEntry>,
    pub refresh_rate_only: Vec<VirtualDisplayModeRemappingEntry>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct VirtualDisplayModeRemappingEntry {
    pub requested_resolution: String,
    pub requested_fps: String,
    pub final_resolution: String,
    pub final_refresh_rate: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDisplayResolverContext {
    pub resolution_is_automatic: bool,
    pub refresh_rate_is_automatic: bool,
    pub client_display_mode_override: bool,
    pub optimize_game_settings: bool,
}

/// Resolved display-mode policy forwarded to the existing topology transaction.
/// `context` is supplied by the stream admission owner and is never inferred by
/// the display provider.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDisplayModePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_option: Option<VirtualDisplayConfigurationOption>,
    #[serde(default)]
    pub remapping: VirtualDisplayModeRemapping,
    pub context: VirtualDisplayResolverContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_option: Option<VirtualDisplayResolutionOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_rate_option: Option<VirtualDisplayRefreshRateOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_refresh_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_option: Option<VirtualDisplayHdrOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_client_hdr_request: Option<bool>,
}

impl VirtualDisplayModePolicy {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .manual_resolution
            .as_deref()
            .is_some_and(|value| !valid_manual_resolution(value))
            || self
                .manual_refresh_rate
                .as_deref()
                .is_some_and(|value| !valid_manual_refresh_rate(value))
            || (self.refresh_rate_option == Some(VirtualDisplayRefreshRateOption::Manual)
                && self
                    .manual_refresh_rate
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty()))
            || (self.context.optimize_game_settings
                && !self.context.client_display_mode_override
                && self.resolution_option == Some(VirtualDisplayResolutionOption::Manual)
                && self
                    .manual_resolution
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty()))
            || (self.hdr_option == Some(VirtualDisplayHdrOption::Auto)
                && self.effective_client_hdr_request.is_none())
            || !valid_remapping_group("mixed", &self.remapping.mixed)
            || !valid_remapping_group("resolution_only", &self.remapping.resolution_only)
            || !valid_remapping_group("refresh_rate_only", &self.remapping.refresh_rate_only)
        {
            return Err("invalid_virtual_display_policy");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayMode {
    Disabled,
    PerClient,
    Shared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualDisplayLayout {
    Exclusive,
    Extended,
    ExtendedPrimary,
    ExtendedIsolated,
    ExtendedPrimaryIsolated,
}

/// One authenticated, fully resolved stream-time virtual-display request.
///
/// `identity` is the stable provider key for one monitor specification and may
/// be shared by several streams. `reference_id` is private per-stream ownership
/// used to release only that stream's share of the monitor lease.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualDisplayIntent {
    pub identity: String,
    pub reference_id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_millihz: u32,
    pub dd_virtual_display_scale: i32,
    pub dd_activate_virtual_display: bool,
    pub dd_use_sunshine_virtual_display_driver: bool,
    pub virtual_display_mode: VirtualDisplayMode,
    pub virtual_display_layout: VirtualDisplayLayout,
    /// Optional frozen result of launch/display precedence. `None` is the
    /// legacy interpretation; `Some(false)` preserves an explicit physical
    /// selection even when the inherited mode requests a virtual display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_virtual_display: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_policy: Option<VirtualDisplayModePolicy>,
}

impl VirtualDisplayIntent {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_identity(&self.identity)
            || !valid_reference_id(&self.reference_id)
            || self.name.is_empty()
            || self.name.len() > 31
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
            || !(320..=7680).contains(&self.width)
            || !(200..=4320).contains(&self.height)
            || !(1000..=1_000_000).contains(&self.refresh_millihz)
            || !matches!(
                self.dd_virtual_display_scale,
                -1 | 0 | 100 | 125 | 150 | 175 | 200 | 225 | 250 | 300 | 350 | 400 | 450 | 500
            )
        {
            return Err("invalid_virtual_display_intent");
        }
        if self
            .display_policy
            .as_ref()
            .is_some_and(|policy| policy.validate().is_err())
        {
            return Err("invalid_virtual_display_policy");
        }
        Ok(())
    }

    /// Return the pre-resolved request when available. Older wire values use
    /// the source-compatible mode/auto-activation fallback.
    pub fn active(&self) -> bool {
        self.request_virtual_display.unwrap_or_else(|| {
            self.dd_activate_virtual_display
                || self.virtual_display_mode != VirtualDisplayMode::Disabled
        })
    }
}

pub fn valid_identity(identity: &str) -> bool {
    identity.len() == 36
        && identity.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && identity.bytes().any(|byte| byte != b'0' && byte != b'-')
}

pub fn valid_reference_id(reference_id: &str) -> bool {
    !reference_id.is_empty()
        && reference_id.len() <= 128
        && reference_id.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_manual_resolution(value: &str) -> bool {
    if value.len() > 32 || value.chars().any(char::is_control) {
        return false;
    }
    let value = value.trim().replace('×', "x");
    if value.is_empty() {
        return true;
    }
    value.split_once('x').is_some_and(|(width, height)| {
        !width.is_empty()
            && !height.is_empty()
            && !height.contains('x')
            && width.bytes().all(|byte| byte.is_ascii_digit())
            && height.bytes().all(|byte| byte.is_ascii_digit())
            && width.parse::<u32>().is_ok()
            && height.parse::<u32>().is_ok()
    })
}

fn valid_manual_refresh_rate(value: &str) -> bool {
    if value.len() > 64 || value.chars().any(char::is_control) {
        return false;
    }
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || (!fraction.is_empty() && !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        || (value.contains('.') && fraction.is_empty())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let Ok(integer) = whole.parse::<u32>() else {
        return false;
    };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return true;
    }
    if fraction.len() > 31 {
        return false;
    }
    let (Ok(numerator), Some(denominator)) = (
        fraction.parse::<u128>(),
        10_u128.checked_pow(fraction.len() as u32),
    ) else {
        return false;
    };
    let mut left = numerator;
    let mut right = denominator;
    while right != 0 {
        (left, right) = (right, left % right);
    }
    let numerator = numerator / left;
    let denominator = denominator / left;
    u32::try_from(denominator).is_ok()
        && u128::from(integer)
            .checked_mul(denominator)
            .and_then(|scaled| scaled.checked_add(numerator))
            .is_some_and(|combined| u32::try_from(combined).is_ok())
}

fn valid_remapping_group(kind: &str, entries: &[VirtualDisplayModeRemappingEntry]) -> bool {
    entries.len() <= 128
        && entries.iter().all(|entry| {
            let values = [
                &entry.requested_resolution,
                &entry.requested_fps,
                &entry.final_resolution,
                &entry.final_refresh_rate,
            ];
            values
                .iter()
                .all(|value| value.len() <= 64 && !value.chars().any(char::is_control))
                && valid_remapping_resolution(&entry.requested_resolution)
                && valid_remapping_resolution(&entry.final_resolution)
                && valid_remapping_rate(&entry.requested_fps, false)
                && valid_remapping_rate(&entry.final_refresh_rate, true)
                && match kind {
                    "mixed" => {
                        !entry.final_resolution.trim().is_empty()
                            || !entry.final_refresh_rate.trim().is_empty()
                    }
                    "resolution_only" => !entry.final_resolution.trim().is_empty(),
                    "refresh_rate_only" => !entry.final_refresh_rate.trim().is_empty(),
                    _ => false,
                }
        })
}

fn valid_remapping_resolution(value: &str) -> bool {
    let value = value.trim().replace('×', "x");
    if value.is_empty() {
        return true;
    }
    value.split_once('x').is_some_and(|(width, height)| {
        !width.is_empty()
            && !height.is_empty()
            && !height.contains('x')
            && width.bytes().all(|byte| byte.is_ascii_digit())
            && height.bytes().all(|byte| byte.is_ascii_digit())
            && width.parse::<u32>().is_ok()
            && height.parse::<u32>().is_ok()
    })
}

fn valid_remapping_rate(value: &str, allow_fraction: bool) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    let Some((whole, fraction)) = value.split_once('.') else {
        return value.bytes().all(|byte| byte.is_ascii_digit()) && value.parse::<u32>().is_ok();
    };
    if !allow_fraction
        || whole.is_empty()
        || fraction.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return whole.parse::<u32>().is_ok();
    }
    format!("{whole}{fraction}").parse::<u32>().is_ok()
        && u32::try_from(fraction.len())
            .ok()
            .and_then(|length| 10_u32.checked_pow(length))
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn intent() -> VirtualDisplayIntent {
        VirtualDisplayIntent {
            identity: "00000000-0000-0000-0000-000000000001".into(),
            reference_id: "stream-session-42".into(),
            name: "Amaterasu".into(),
            width: 1920,
            height: 1080,
            refresh_millihz: 60000,
            dd_virtual_display_scale: 0,
            dd_activate_virtual_display: true,
            dd_use_sunshine_virtual_display_driver: true,
            virtual_display_mode: VirtualDisplayMode::PerClient,
            virtual_display_layout: VirtualDisplayLayout::Exclusive,
            request_virtual_display: None,
            display_policy: None,
        }
    }

    #[test]
    fn resolved_intent_is_typed_bounded_and_strict() {
        let valid = intent();
        assert_eq!(valid.validate(), Ok(()));
        assert!(valid.active());
        for mode in ["disabled", "per_client", "shared"] {
            let mut value = serde_json::to_value(&valid).unwrap();
            value["virtual_display_mode"] = json!(mode);
            let parsed: VirtualDisplayIntent = serde_json::from_value(value).unwrap();
            assert_eq!(parsed.validate(), Ok(()));
            assert_eq!(
                parsed.active(),
                mode != "disabled" || parsed.dd_activate_virtual_display
            );
        }
        for layout in [
            "exclusive",
            "extended",
            "extended_primary",
            "extended_isolated",
            "extended_primary_isolated",
        ] {
            let mut value = serde_json::to_value(&valid).unwrap();
            value["virtual_display_layout"] = json!(layout);
            assert!(
                serde_json::from_value::<VirtualDisplayIntent>(value)
                    .unwrap()
                    .validate()
                    .is_ok()
            );
        }
    }

    #[test]
    fn resolved_intent_rejects_bad_identity_scale_dimensions_and_unknown_fields() {
        let mut value = serde_json::to_value(intent()).unwrap();
        for (field, invalid) in [
            ("identity", json!("not-a-uuid")),
            ("reference_id", json!("stream\n42")),
            ("width", json!(319)),
            ("height", json!(4321)),
            ("refresh_millihz", json!(0)),
            ("dd_virtual_display_scale", json!(126)),
        ] {
            let mut invalid_value = value.clone();
            invalid_value[field] = invalid;
            let parsed: VirtualDisplayIntent = serde_json::from_value(invalid_value).unwrap();
            assert_eq!(parsed.validate(), Err("invalid_virtual_display_intent"));
        }
        value["unexpected"] = json!(true);
        assert!(serde_json::from_value::<VirtualDisplayIntent>(value).is_err());
    }

    #[test]
    fn legacy_explicit_modes_activate_and_flag_is_an_additional_auto_route() {
        let mut per_client_without_auto_activation = intent();
        per_client_without_auto_activation.dd_activate_virtual_display = false;
        assert!(per_client_without_auto_activation.active());

        let mut shared_without_auto_activation = intent();
        shared_without_auto_activation.dd_activate_virtual_display = false;
        shared_without_auto_activation.virtual_display_mode = VirtualDisplayMode::Shared;
        assert!(shared_without_auto_activation.active());

        let mut disabled_without_auto_activation = intent();
        disabled_without_auto_activation.dd_activate_virtual_display = false;
        disabled_without_auto_activation.virtual_display_mode = VirtualDisplayMode::Disabled;
        assert!(!disabled_without_auto_activation.active());

        let mut auto_activation_with_disabled_mode = intent();
        auto_activation_with_disabled_mode.virtual_display_mode = VirtualDisplayMode::Disabled;
        assert!(auto_activation_with_disabled_mode.active());
    }

    #[test]
    fn frozen_request_preserves_physical_and_virtual_selection() {
        let mut explicit_physical = intent();
        explicit_physical.request_virtual_display = Some(false);
        assert!(!explicit_physical.active());

        let mut explicit_virtual = intent();
        explicit_virtual.virtual_display_mode = VirtualDisplayMode::Disabled;
        explicit_virtual.dd_activate_virtual_display = false;
        explicit_virtual.request_virtual_display = Some(true);
        assert!(explicit_virtual.active());

        let mut encoded = serde_json::to_value(&explicit_physical).unwrap();
        encoded["request_virtual_display"] = json!("false");
        assert!(serde_json::from_value::<VirtualDisplayIntent>(encoded).is_err());
    }

    #[test]
    fn resolved_display_policy_round_trips_strictly_and_bounds_remapping() {
        let mut request = intent();
        request.display_policy = Some(VirtualDisplayModePolicy {
            configuration_option: Some(VirtualDisplayConfigurationOption::EnsurePrimary),
            remapping: VirtualDisplayModeRemapping {
                mixed: vec![VirtualDisplayModeRemappingEntry {
                    requested_fps: "60".into(),
                    final_resolution: "1920x1080".into(),
                    final_refresh_rate: "59.94".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            context: VirtualDisplayResolverContext {
                resolution_is_automatic: true,
                refresh_rate_is_automatic: true,
                client_display_mode_override: false,
                optimize_game_settings: true,
            },
            resolution_option: Some(VirtualDisplayResolutionOption::Manual),
            manual_resolution: Some("1920×1080".into()),
            refresh_rate_option: Some(VirtualDisplayRefreshRateOption::Manual),
            manual_refresh_rate: Some("59.94".into()),
            hdr_option: Some(VirtualDisplayHdrOption::Auto),
            effective_client_hdr_request: Some(true),
        });
        assert_eq!(request.validate(), Ok(()));

        let encoded = serde_json::to_value(&request).unwrap();
        let decoded: VirtualDisplayIntent = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, request);
        assert_eq!(
            encoded["display_policy"]["configuration_option"],
            "ensure_primary"
        );
        assert_eq!(encoded["display_policy"]["hdr_option"], "auto");

        let mut unknown = encoded.clone();
        unknown["display_policy"]["unsupported"] = serde_json::json!(true);
        assert!(serde_json::from_value::<VirtualDisplayIntent>(unknown).is_err());

        let mut invalid = request;
        invalid
            .display_policy
            .as_mut()
            .unwrap()
            .remapping
            .mixed
            .push(VirtualDisplayModeRemappingEntry {
                requested_fps: "60".into(),
                ..Default::default()
            });
        assert_eq!(invalid.validate(), Err("invalid_virtual_display_policy"));
    }

    #[test]
    fn automatic_hdr_requires_resolved_client_request_and_manual_modes_require_valid_values() {
        let mut request = intent();
        request.display_policy = Some(VirtualDisplayModePolicy {
            configuration_option: None,
            remapping: VirtualDisplayModeRemapping::default(),
            context: VirtualDisplayResolverContext {
                resolution_is_automatic: false,
                refresh_rate_is_automatic: false,
                client_display_mode_override: false,
                optimize_game_settings: true,
            },
            resolution_option: Some(VirtualDisplayResolutionOption::Manual),
            manual_resolution: Some("1920x1080".into()),
            refresh_rate_option: Some(VirtualDisplayRefreshRateOption::Manual),
            manual_refresh_rate: Some("59.94".into()),
            hdr_option: Some(VirtualDisplayHdrOption::Auto),
            effective_client_hdr_request: None,
        });
        assert_eq!(request.validate(), Err("invalid_virtual_display_policy"));

        let policy = request.display_policy.as_mut().unwrap();
        policy.effective_client_hdr_request = Some(false);
        policy.manual_resolution = Some("1920 by 1080".into());
        assert_eq!(request.validate(), Err("invalid_virtual_display_policy"));
    }
}
