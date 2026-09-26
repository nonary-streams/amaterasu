use crate::configuration_option::DisplayConfigurationOption;
use crate::hdr_option::{DisplayHdrOption, resolve_hdr_configuration};
use crate::mode_remapping::{DisplayMode, ModeRemapping, Resolution, ResolverContext};
use crate::refresh_rate_option::{RefreshRateOption, parse_manual_refresh_rate};
use crate::resolution_option::{ManualResolution, ResolutionOption, select_additional_resolution};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_DISPLAYS: usize = 16;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Snapshot,
    Enumerate {
        display: Option<String>,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Apply {
        configuration: Configuration,
        #[serde(default)]
        expected_topology_revision: Option<String>,
        #[serde(default)]
        operation_id: Option<String>,
        #[serde(default)]
        mode_policy: Option<ModePolicy>,
        #[serde(default)]
        mode_policies: Option<Vec<ModePolicy>>,
    },
    Restore {
        lease: String,
    },
    Recover {
        lease: String,
    },
}
/// Session-selected mode policy for one display in a concrete configuration.
/// The caller supplies client/launch facts; the provider does not infer them.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModePolicy {
    pub display_id: String,
    #[serde(default)]
    pub configuration_option: Option<DisplayConfigurationOption>,
    pub remapping: ModeRemapping,
    pub context: ResolverContext,
    #[serde(default)]
    pub resolution_option: Option<ResolutionOption>,
    #[serde(default)]
    pub manual_resolution: Option<String>,
    #[serde(default)]
    pub refresh_rate_option: Option<RefreshRateOption>,
    #[serde(default)]
    pub manual_refresh_rate: Option<String>,
    #[serde(default)]
    pub hdr_option: Option<DisplayHdrOption>,
    #[serde(default)]
    pub effective_client_hdr_request: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub displays: Vec<DisplaySetting>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplaySetting {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub refresh: Rational,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub rotation: Rotation,
    /// Requested HDR state; None leaves the current state untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_enabled: Option<bool>,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Identity,
    Rotate90,
    Rotate180,
    Rotate270,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rational {
    pub numerator: u32,
    pub denominator: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvailableMode {
    pub width: u32,
    pub height: u32,
    pub refresh: Rational,
    pub nominal_refresh_hz: u32,
    pub bits_per_pixel: u32,
    pub exact_current: bool,
}
impl Rational {
    #[cfg(any(windows, test))]
    pub fn equivalent(self, other: Self) -> bool {
        u64::from(self.numerator) * u64::from(other.denominator)
            == u64::from(other.numerator) * u64::from(self.denominator)
    }
}
impl Configuration {
    pub fn resolve_mode_policy(
        &mut self,
        policy: &ModePolicy,
        available_modes: Option<&[AvailableMode]>,
    ) -> Result<(), DisplayError> {
        self.validate()?;
        self.resolve_mode_policy_in_batch(policy, available_modes)?;
        self.validate()
    }

    /// Resolve one target while the caller owns validation of the complete batch.
    pub(crate) fn resolve_mode_policy_in_batch(
        &mut self,
        policy: &ModePolicy,
        available_modes: Option<&[AvailableMode]>,
    ) -> Result<(), DisplayError> {
        let display = self
            .displays
            .iter_mut()
            .find(|display| display.id.eq_ignore_ascii_case(&policy.display_id))
            .ok_or_else(|| {
                DisplayError::new(
                    "invalid_request",
                    "mode policy display is not in the configuration",
                )
            })?;
        let requested = DisplayMode {
            resolution: Some(Resolution {
                width: display.width,
                height: display.height,
            }),
            refresh_rate: Some(display.refresh),
        };
        let mut selected = requested;
        let mut context = policy.context;
        if !context.client_display_mode_override {
            if let Some(option) = policy.resolution_option {
                context.resolution_is_automatic = option.is_automatic();
                let manual = if context.optimize_game_settings {
                    ManualResolution::parse(policy.manual_resolution.as_deref().unwrap_or_default())
                        .map_err(|error| DisplayError::new("invalid_request", error.to_string()))?
                } else {
                    ManualResolution::Unspecified
                };
                if let Some(resolution) = select_additional_resolution(
                    option,
                    manual,
                    requested.resolution,
                    context.optimize_game_settings,
                )
                .map_err(|error| DisplayError::new("invalid_request", error.to_string()))?
                {
                    selected.resolution = Some(resolution);
                }
            }
            if let Some(option) = policy.refresh_rate_option {
                context.refresh_rate_is_automatic = option.is_automatic();
                match option {
                    RefreshRateOption::Auto | RefreshRateOption::Disabled => {}
                    RefreshRateOption::Manual => {
                        selected.refresh_rate = Some(
                            parse_manual_refresh_rate(
                                policy.manual_refresh_rate.as_deref().unwrap_or_default(),
                            )
                            .map_err(|error| {
                                DisplayError::new(
                                    "invalid_request",
                                    format!("invalid manual refresh rate: {error:?}"),
                                )
                            })?
                            .ok_or_else(|| {
                                DisplayError::new(
                                    "invalid_request",
                                    "manual refresh rate must be specified",
                                )
                            })?,
                        );
                    }
                    RefreshRateOption::PreferHighest => {}
                }
            }
        }
        let mut resolved = policy
            .remapping
            .resolve(context, selected)
            .map_err(|error| {
                DisplayError::new(
                    "invalid_request",
                    format!("invalid display mode remapping: {error}"),
                )
            })?;
        if !context.client_display_mode_override
            && policy.refresh_rate_option == Some(RefreshRateOption::PreferHighest)
        {
            let resolution = resolved.resolution.ok_or_else(|| {
                DisplayError::new("invalid_request", "mode policy has no selected resolution")
            })?;
            let modes = available_modes.ok_or_else(|| {
                DisplayError::new("unsupported_display", "display modes are unavailable")
            })?;
            let highest = modes
                .iter()
                .filter(|mode| {
                    mode.width == resolution.width
                        && mode.height == resolution.height
                        && mode.bits_per_pixel == 32
                })
                .filter(|mode| mode.refresh.denominator != 0 && mode.refresh.numerator != 0)
                .max_by(|left, right| {
                    left.nominal_refresh_hz
                        .cmp(&right.nominal_refresh_hz)
                        .then_with(|| left.exact_current.cmp(&right.exact_current))
                        .then_with(|| {
                            (u64::from(left.refresh.numerator)
                                * u64::from(right.refresh.denominator))
                            .cmp(
                                &(u64::from(right.refresh.numerator)
                                    * u64::from(left.refresh.denominator)),
                            )
                        })
                })
                .ok_or_else(|| {
                    DisplayError::new(
                        "unsupported_display",
                        format!(
                            "display has no available 32bpp mode at {}x{}",
                            resolution.width, resolution.height
                        ),
                    )
                })?;
            resolved.refresh_rate = Some(highest.refresh);
        }
        if let Some(resolution) = resolved.resolution {
            display.width = resolution.width;
            display.height = resolution.height;
        }
        if let Some(refresh_rate) = resolved.refresh_rate {
            display.refresh = refresh_rate;
        }
        if let Some(option) = policy.hdr_option {
            display.hdr_enabled = match option {
                DisplayHdrOption::Auto => Some(
                    resolve_hdr_configuration(
                        option,
                        policy.effective_client_hdr_request.ok_or_else(|| {
                            DisplayError::new(
                                "invalid_request",
                                "auto HDR policy requires the effective client HDR request",
                            )
                        })?,
                    )
                    .expect("auto HDR resolves to a state"),
                ),
                DisplayHdrOption::Disabled => None,
            };
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.displays.is_empty() || self.displays.len() > MAX_DISPLAYS {
            return Err(DisplayError::new(
                "invalid_request",
                "configuration must contain 1..16 displays",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut primary = false;
        for display in &self.displays {
            if display.id.is_empty()
                || display.id.len() > 1024
                || display.id.chars().any(char::is_control)
                || !ids.insert(display.id.to_lowercase())
                || !(320..=16_384).contains(&display.width)
                || !(200..=16_384).contains(&display.height)
                || display.refresh.denominator == 0
                || display.refresh.numerator == 0
                || display.refresh.numerator > 1_000_000
                || display.refresh.denominator > 100_000
                || display.x.abs_diff(0) > 65_536
                || display.y.abs_diff(0) > 65_536
            {
                return Err(DisplayError::new(
                    "invalid_request",
                    "invalid display identity, size, rate, position or duplicate display",
                ));
            }
            let hz = f64::from(display.refresh.numerator) / f64::from(display.refresh.denominator);
            if !(1.0..=1000.0).contains(&hz) {
                return Err(DisplayError::new(
                    "invalid_request",
                    "refresh rate must be 1..1000 Hz",
                ));
            }
            primary |= display.x == 0 && display.y == 0;
        }
        if !primary {
            return Err(DisplayError::new(
                "invalid_request",
                "one display must begin at the primary origin (0,0)",
            ));
        }
        for (index, left) in self.displays.iter().enumerate() {
            for right in self.displays.iter().skip(index + 1) {
                if i64::from(left.x) < i64::from(right.x) + i64::from(right.width)
                    && i64::from(right.x) < i64::from(left.x) + i64::from(left.width)
                    && i64::from(left.y) < i64::from(right.y) + i64::from(right.height)
                    && i64::from(right.y) < i64::from(left.y) + i64::from(left.height)
                    && (left.x != right.x
                        || left.y != right.y
                        || left.width != right.width
                        || left.height != right.height)
                {
                    return Err(DisplayError::new(
                        "unsupported_topology",
                        "requested display rectangles must be disjoint or exactly cloned",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Address {
    pub adapter_low: u32,
    pub adapter_high: i32,
    pub id: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMode {
    pub width: u32,
    pub height: u32,
    pub pixel_format: i32,
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub pixel_rate: u64,
    pub horizontal_sync: Rational,
    pub vertical_sync: Rational,
    pub active_width: u32,
    pub active_height: u32,
    pub total_width: u32,
    pub total_height: u32,
    pub video_standard: u32,
    pub scan_line_ordering: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPath {
    pub id: String,
    pub source_name: String,
    pub source_address: Address,
    pub target_address: Address,
    pub source: SourceMode,
    pub signal: Signal,
    pub rotation: i32,
    pub scaling: i32,
    pub refresh: Rational,
    pub scan_line_ordering: i32,
    pub output_technology: i32,
    pub source_flags: u32,
    pub target_flags: u32,
    pub path_flags: u32,
    /// None denotes a target without a queryable HDR state (including old journals).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_enabled: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub format_version: u32,
    pub paths: Vec<DisplayPath>,
}
impl Snapshot {
    /// Old journals were created only from SDR paths and have no HDR field.
    /// Refuse to overwrite a path whose HDR state changed after that snapshot.
    #[cfg(any(windows, test))]
    pub fn legacy_hdr_target_conflicts(&self, display_id: &str, current: Option<bool>) -> bool {
        current == Some(true)
            && self
                .paths
                .iter()
                .any(|path| path.hdr_enabled.is_none() && path.id.eq_ignore_ascii_case(display_id))
    }

    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.format_version != 1 || self.paths.is_empty() || self.paths.len() > MAX_DISPLAYS {
            return Err(DisplayError::new(
                "invalid_snapshot",
                "unsupported or oversized display snapshot",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for path in &self.paths {
            if path.id.is_empty()
                || path.id.len() > 1024
                || !ids.insert(path.id.to_lowercase())
                || !targets.insert((
                    path.target_address.adapter_low,
                    path.target_address.adapter_high,
                    path.target_address.id,
                ))
                || path.source_name.is_empty()
                || path.source_name.len() > 128
                || path.source.width == 0
                || path.source.height == 0
                || path.refresh.denominator == 0
                || path.refresh.numerator == 0
                || path.signal.vertical_sync.denominator == 0
                || path.signal.vertical_sync.numerator == 0
                || path.signal.horizontal_sync.denominator == 0
                || path.signal.horizontal_sync.numerator == 0
                || !(1..=4).contains(&path.rotation)
            {
                return Err(DisplayError::new(
                    "invalid_snapshot",
                    "invalid display snapshot path",
                ));
            }
        }
        for (index, left) in self.paths.iter().enumerate() {
            for right in self.paths.iter().skip(index + 1) {
                if left.source_address == right.source_address
                    && (left.source_name != right.source_name || left.source != right.source)
                {
                    return Err(DisplayError::new(
                        "invalid_snapshot",
                        "clone paths disagree about their shared source",
                    ));
                }
                if left.source_address != right.source_address
                    && left.source.x == right.source.x
                    && left.source.y == right.source.y
                    && left.source.width == right.source.width
                    && left.source.height == right.source.height
                {
                    return Err(DisplayError::new(
                        "unsupported_topology",
                        "overlapping paths do not share one CCD source",
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn configuration(&self) -> Result<Configuration, DisplayError> {
        self.validate()?;
        let displays = self
            .paths
            .iter()
            .map(|path| DisplaySetting {
                id: path.id.clone(),
                width: path.source.width,
                height: path.source.height,
                refresh: path.refresh,
                x: path.source.x,
                y: path.source.y,
                rotation: match path.rotation {
                    1 => Rotation::Identity,
                    2 => Rotation::Rotate90,
                    3 => Rotation::Rotate180,
                    4 => Rotation::Rotate270,
                    _ => unreachable!("validated rotation"),
                },
                hdr_enabled: path.hdr_enabled,
            })
            .collect();
        let configuration = Configuration { displays };
        configuration.validate().map_err(|_| {
            DisplayError::new(
                "unsupported_topology",
                "active topology cannot form a complete configuration",
            )
        })?;
        Ok(configuration)
    }
    pub fn revision(&self) -> Result<String, DisplayError> {
        self.validate()?;
        let mut paths = self.paths.clone();
        paths.sort_by_key(|path| path.id.to_lowercase());
        let mut bytes = serde_json::to_vec(&paths)
            .map_err(|error| DisplayError::new("invalid_snapshot", error.to_string()))?;
        // Independent CCD paths may be reported in a different order without
        // changing the desktop. Clone path order is meaningful priority, so
        // bind it (and its position relative to independent paths) into CAS.
        let clone_priority = self.clone_priority();
        if !clone_priority.is_empty() {
            bytes.extend_from_slice(b"|clone-priority-v1|");
            bytes.extend(
                serde_json::to_vec(&clone_priority)
                    .map_err(|error| DisplayError::new("invalid_snapshot", error.to_string()))?,
            );
        }
        // Two independent noncryptographic lanes make a compact opaque CCD state token.
        let mut a = 0xcbf29ce484222325_u64;
        let mut b = 0x84222325cbf29ce4_u64;
        for byte in bytes {
            a = (a ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            b = (b ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        Ok(format!("{a:016x}{b:016x}"))
    }
    pub(crate) fn clone_priority(&self) -> Vec<(String, Vec<String>)> {
        let is_clone = |path: &DisplayPath| {
            self.paths
                .iter()
                .filter(|peer| peer.source_address == path.source_address)
                .count()
                > 1
        };
        self.paths
            .iter()
            .enumerate()
            .filter(|(_, path)| is_clone(path))
            .map(|(index, path)| {
                let mut independent_before: Vec<_> = self.paths[..index]
                    .iter()
                    .filter(|earlier| !is_clone(earlier))
                    .map(|earlier| earlier.id.to_lowercase())
                    .collect();
                independent_before.sort();
                (path.id.to_lowercase(), independent_before)
            })
            .collect()
    }
    #[cfg(any(windows, test))]
    pub fn equivalent(&self, other: &Self) -> bool {
        self.validate().is_ok()
            && other.validate().is_ok()
            && self.paths.len() == other.paths.len()
            && self.paths.iter().all(|left| {
                other.paths.iter().any(|right| {
                    left.id == right.id
                        && left.source_name == right.source_name
                        && self.paths.iter().all(|peer| {
                            other
                                .paths
                                .iter()
                                .find(|candidate| candidate.id == peer.id)
                                .is_some_and(|candidate| {
                                    (left.source_address == peer.source_address)
                                        == (right.source_address == candidate.source_address)
                                })
                        })
                        && left.source == right.source
                        && left.signal == right.signal
                        && left.rotation == right.rotation
                        && left.scaling == right.scaling
                        && left.refresh.equivalent(right.refresh)
                        && left.scan_line_ordering == right.scan_line_ordering
                        && left.output_technology == right.output_technology
                        && left.path_flags == right.path_flags
                        && (left.hdr_enabled == right.hdr_enabled
                            || (left.hdr_enabled.is_none() && right.hdr_enabled == Some(false)))
                })
            })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub owner: String,
    pub lease: String,
    pub snapshot: Snapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_configuration: Option<Configuration>,
    /// Parsed caller request before topology composition or mode resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_identity: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_topology_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_route: Option<RouteBinding>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteBinding {
    pub id: String,
    pub source_name: String,
    pub source_address: Address,
    pub target_address: Address,
    pub output_technology: i32,
}
impl Journal {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.owner.is_empty()
            || self.owner.len() > 128
            || self.lease.is_empty()
            || self.lease.len() > 128
            || self.operation_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 128 || !id.bytes().all(|byte| byte.is_ascii_graphic())
            })
            || self
                .requested_configuration
                .as_ref()
                .is_some_and(|configuration| configuration.validate().is_err())
            || !self.valid_request_identity()
            || self.pending_route.as_ref().is_some_and(|route| {
                route.id.is_empty()
                    || route.id.len() > 1024
                    || route.source_name.is_empty()
                    || route.source_name.len() > 128
            })
        {
            return Err(DisplayError::new(
                "invalid_journal",
                "invalid display journal owner or lease",
            ));
        }
        self.snapshot.validate()
    }

    fn valid_request_identity(&self) -> bool {
        let Some(identity) = &self.request_identity else {
            return true;
        };
        let Ok(operation @ Operation::Apply { .. }) =
            serde_json::from_value::<Operation>(identity.clone())
        else {
            return false;
        };
        let Operation::Apply {
            operation_id,
            expected_topology_revision,
            ..
        } = &operation
        else {
            unreachable!("parsed Apply request")
        };
        operation_id == &self.operation_id
            && expected_topology_revision == &self.expected_topology_revision
            && serde_json::to_value(&operation).ok().as_ref() == Some(identity)
    }
}
#[derive(Debug)]
pub struct DisplayError {
    pub may_have_changed: bool,
    pub code: &'static str,
    pub message: String,
}
impl DisplayError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            may_have_changed: false,
            code,
            message: message.into(),
        }
    }
    #[cfg(any(windows, test))]
    pub fn after_mutation(mut self) -> Self {
        self.may_have_changed = true;
        self
    }
    pub fn payload(self) -> Value {
        json!({"ok":false,"error":{"code":self.code,"message":self.message.chars().take(512).collect::<String>()}})
    }
}
pub type DisplayResult<T> = Result<T, DisplayError>;
