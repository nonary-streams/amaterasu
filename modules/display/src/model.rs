use crate::mode_remapping::{DisplayMode, ModeRemapping, Resolution, ResolverContext};
use crate::refresh_rate_option::{RefreshRateOption, parse_manual_refresh_rate};
use crate::resolution_option::{ManualResolution, ResolutionOption, select_additional_resolution};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_DISPLAYS: usize = 16;
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Enumerate {
        display: Option<String>,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    Apply {
        configuration: Configuration,
        #[serde(default)]
        mode_policy: Option<ModePolicy>,
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
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModePolicy {
    pub display_id: String,
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
        self.validate()
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
                {
                    return Err(DisplayError::new(
                        "unsupported_topology",
                        "requested extended display rectangles must not overlap; clone requests are not supported",
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
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub format_version: u32,
    pub paths: Vec<DisplayPath>,
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.format_version != 1 || self.paths.is_empty() || self.paths.len() > MAX_DISPLAYS {
            return Err(DisplayError::new(
                "invalid_snapshot",
                "unsupported or oversized display snapshot",
            ));
        }
        let mut ids = BTreeSet::new();
        for path in &self.paths {
            if path.id.is_empty()
                || path.id.len() > 1024
                || !ids.insert(&path.id)
                || path.source_name.is_empty()
                || path.source_name.len() > 128
                || path.source.width == 0
                || path.source.height == 0
                || path.refresh.denominator == 0
                || path.signal.vertical_sync.denominator == 0
                || path.signal.horizontal_sync.denominator == 0
                || !(1..=4).contains(&path.rotation)
            {
                return Err(DisplayError::new(
                    "invalid_snapshot",
                    "invalid display snapshot path",
                ));
            }
        }
        Ok(())
    }
    #[cfg(any(windows, test))]
    pub fn equivalent(&self, other: &Self) -> bool {
        self.paths.len() == other.paths.len()
            && self.paths.iter().zip(&other.paths).all(|(left, right)| {
                left.id == right.id
                    && left.source_name == right.source_name
                    && left.source == right.source
                    && left.signal == right.signal
                    && left.rotation == right.rotation
                    && left.scaling == right.scaling
                    && left.refresh.equivalent(right.refresh)
                    && left.scan_line_ordering == right.scan_line_ordering
            })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub owner: String,
    pub lease: String,
    pub snapshot: Snapshot,
}
impl Journal {
    pub fn validate(&self) -> Result<(), DisplayError> {
        if self.owner.is_empty()
            || self.owner.len() > 128
            || self.lease.is_empty()
            || self.lease.len() > 128
        {
            return Err(DisplayError::new(
                "invalid_journal",
                "invalid display journal owner or lease",
            ));
        }
        self.snapshot.validate()
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
