//! Pure policy for mapping a client's requested display mode.
use crate::Rational;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The three independent rule lists used by display-mode remapping.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModeRemapping {
    pub mixed: Vec<ModeRemappingEntry>,
    pub resolution_only: Vec<ModeRemappingEntry>,
    pub refresh_rate_only: Vec<ModeRemappingEntry>,
}

/// A single request matcher and its optional final values.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModeRemappingEntry {
    pub requested_resolution: String,
    pub requested_fps: String,
    pub final_resolution: String,
    pub final_refresh_rate: String,
}

/// A width and height parsed from a remapping rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// The optional resolution and refresh values before or after remapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayMode {
    pub resolution: Option<Resolution>,
    pub refresh_rate: Option<Rational>,
}

/// Runtime facts that control which remapping list may apply.
///
/// Callers must provide these explicitly because option selection, client mode
/// overrides, and the client's "Optimize game settings" choice are not config
/// defaults owned by this policy.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResolverContext {
    pub resolution_is_automatic: bool,
    pub refresh_rate_is_automatic: bool,
    pub client_display_mode_override: bool,
    pub optimize_game_settings: bool,
}

/// An active rule could not be parsed or would not produce an output value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeRemappingError {
    InvalidField {
        entry_index: usize,
        field: &'static str,
    },
    MissingFinalOutput {
        entry_index: usize,
    },
}

impl fmt::Display for ModeRemappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField { entry_index, field } => {
                write!(formatter, "entry {entry_index} has an invalid {field}")
            }
            Self::MissingFinalOutput { entry_index } => {
                write!(
                    formatter,
                    "entry {entry_index} has no applicable final value"
                )
            }
        }
    }
}

impl std::error::Error for ModeRemappingError {}

impl ModeRemapping {
    /// Resolve the first matching rule for the selected automatic options.
    ///
    /// A client display-mode override bypasses all mapping. Empty request
    /// fields match any value, and empty final fields leave that value alone.
    pub fn resolve(
        &self,
        context: ResolverContext,
        requested_mode: DisplayMode,
    ) -> Result<DisplayMode, ModeRemappingError> {
        if context.client_display_mode_override {
            return Ok(requested_mode);
        }

        let (kind, entries) =
            if context.resolution_is_automatic && context.refresh_rate_is_automatic {
                (RemappingKind::Mixed, &self.mixed)
            } else if context.resolution_is_automatic {
                (RemappingKind::ResolutionOnly, &self.resolution_only)
            } else if context.refresh_rate_is_automatic {
                (RemappingKind::RefreshRateOnly, &self.refresh_rate_only)
            } else {
                return Ok(requested_mode);
            };

        for (entry_index, entry) in entries.iter().enumerate() {
            // The pinned configuration contract gates rules that use resolution
            // fields on the explicit client-side optimization setting.
            if !context.optimize_game_settings
                && (!entry.requested_resolution.trim().is_empty()
                    || !entry.final_resolution.trim().is_empty())
            {
                continue;
            }

            let parsed = parse_entry(kind, entry, entry_index)?;
            if parsed.final_resolution.is_none() && parsed.final_refresh_rate.is_none() {
                return Err(ModeRemappingError::MissingFinalOutput { entry_index });
            }

            if parsed
                .requested_resolution
                .is_some_and(|resolution| Some(resolution) != requested_mode.resolution)
                || parsed.requested_fps.is_some_and(|rate| {
                    !requested_mode
                        .refresh_rate
                        .is_some_and(|v| rational_equal(rate, v))
                })
            {
                continue;
            }

            let mut resolved_mode = requested_mode;
            if let Some(resolution) = parsed.final_resolution {
                resolved_mode.resolution = Some(resolution);
            }
            if let Some(refresh_rate) = parsed.final_refresh_rate {
                resolved_mode.refresh_rate = Some(refresh_rate);
            }
            return Ok(resolved_mode);
        }

        Ok(requested_mode)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemappingKind {
    Mixed,
    ResolutionOnly,
    RefreshRateOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ParsedEntry {
    requested_resolution: Option<Resolution>,
    requested_fps: Option<Rational>,
    final_resolution: Option<Resolution>,
    final_refresh_rate: Option<Rational>,
}

impl RemappingKind {
    fn maps_resolution(self) -> bool {
        matches!(self, Self::Mixed | Self::ResolutionOnly)
    }

    fn maps_refresh_rate(self) -> bool {
        matches!(self, Self::Mixed | Self::RefreshRateOnly)
    }
}

fn parse_entry(
    kind: RemappingKind,
    entry: &ModeRemappingEntry,
    entry_index: usize,
) -> Result<ParsedEntry, ModeRemappingError> {
    let mut parsed = ParsedEntry::default();
    if kind.maps_resolution() {
        parsed.requested_resolution = parse_resolution(
            &entry.requested_resolution,
            entry_index,
            "requested_resolution",
        )?;
        parsed.final_resolution =
            parse_resolution(&entry.final_resolution, entry_index, "final_resolution")?;
    }
    if kind.maps_refresh_rate() {
        parsed.requested_fps =
            parse_rate(&entry.requested_fps, false, entry_index, "requested_fps")?;
        parsed.final_refresh_rate = parse_rate(
            &entry.final_refresh_rate,
            true,
            entry_index,
            "final_refresh_rate",
        )?;
    }
    Ok(parsed)
}

fn parse_resolution(
    value: &str,
    entry_index: usize,
    field: &'static str,
) -> Result<Option<Resolution>, ModeRemappingError> {
    let normalized = value.trim().replace('×', "x");
    if normalized.is_empty() {
        return Ok(None);
    }
    let Some((width, height)) = normalized.split_once('x') else {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    };
    if width.is_empty()
        || height.is_empty()
        || height.contains('x')
        || !width.bytes().all(|byte| byte.is_ascii_digit())
        || !height.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    }
    let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>()) else {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    };
    Ok(Some(Resolution { width, height }))
}

fn parse_rate(
    value: &str,
    allow_fraction: bool,
    entry_index: usize,
    field: &'static str,
) -> Result<Option<Rational>, ModeRemappingError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || parts.next().is_some()
        || (!allow_fraction && fraction.is_some())
        || fraction.is_some_and(|digits| {
            digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    }

    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.unwrap_or_default().trim_end_matches('0');
    if fraction.is_empty() {
        let Ok(numerator) = whole.parse::<u32>() else {
            return Err(ModeRemappingError::InvalidField { entry_index, field });
        };
        return Ok(Some(Rational {
            numerator,
            denominator: 1,
        }));
    }

    let numerator_text = format!("{whole}{fraction}");
    let Ok(numerator) = numerator_text.parse::<u32>() else {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    };
    let Ok(scale) = u32::try_from(fraction.len()) else {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    };
    let Some(denominator) = 10_u32.checked_pow(scale) else {
        return Err(ModeRemappingError::InvalidField { entry_index, field });
    };
    Ok(Some(Rational {
        numerator,
        denominator,
    }))
}

fn rational_equal(left: Rational, right: Rational) -> bool {
    if left.denominator == 0 || right.denominator == 0 {
        return left == right;
    }
    u64::from(left.numerator) * u64::from(right.denominator)
        == u64::from(right.numerator) * u64::from(left.denominator)
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayMode, ModeRemapping, ModeRemappingEntry, ModeRemappingError, Rational, Resolution,
        ResolverContext,
    };
    use serde_json::json;

    fn context(
        resolution_is_automatic: bool,
        refresh_rate_is_automatic: bool,
        optimize_game_settings: bool,
    ) -> ResolverContext {
        ResolverContext {
            resolution_is_automatic,
            refresh_rate_is_automatic,
            client_display_mode_override: false,
            optimize_game_settings,
        }
    }

    fn mode(width: u32, height: u32, numerator: u32, denominator: u32) -> DisplayMode {
        DisplayMode {
            resolution: Some(Resolution { width, height }),
            refresh_rate: Some(Rational {
                numerator,
                denominator,
            }),
        }
    }

    #[test]
    fn configuration_defaults_lists_and_fields_and_rejects_unknown_keys() {
        let defaults: ModeRemapping = serde_json::from_value(json!({})).unwrap();
        assert!(defaults.mixed.is_empty());
        assert!(defaults.resolution_only.is_empty());
        assert!(defaults.refresh_rate_only.is_empty());

        let entry: ModeRemappingEntry = serde_json::from_value(json!({})).unwrap();
        assert_eq!(entry.requested_resolution, "");
        assert_eq!(entry.requested_fps, "");
        assert_eq!(entry.final_resolution, "");
        assert_eq!(entry.final_refresh_rate, "");

        assert!(serde_json::from_value::<ModeRemapping>(json!({"extra": true})).is_err());
        assert!(serde_json::from_value::<ModeRemappingEntry>(json!({"extra": true})).is_err());
    }

    #[test]
    fn automatic_option_pair_selects_its_own_list() {
        let remapping = ModeRemapping {
            mixed: vec![ModeRemappingEntry {
                final_resolution: "2560x1440".into(),
                ..ModeRemappingEntry::default()
            }],
            resolution_only: vec![ModeRemappingEntry {
                final_resolution: "1920x1200".into(),
                ..ModeRemappingEntry::default()
            }],
            refresh_rate_only: vec![ModeRemappingEntry {
                final_refresh_rate: "75".into(),
                ..ModeRemappingEntry::default()
            }],
        };
        let requested = mode(1920, 1080, 60, 1);

        assert_eq!(
            remapping.resolve(context(true, true, true), requested),
            Ok(mode(2560, 1440, 60, 1))
        );
        assert_eq!(
            remapping.resolve(context(true, false, true), requested),
            Ok(mode(1920, 1200, 60, 1))
        );
        assert_eq!(
            remapping.resolve(context(false, true, true), requested),
            Ok(mode(1920, 1080, 75, 1))
        );
        assert_eq!(
            remapping.resolve(context(false, false, true), requested),
            Ok(requested)
        );
    }

    #[test]
    fn first_wildcard_match_keeps_empty_final_fields_unchanged() {
        let remapping = ModeRemapping {
            mixed: vec![
                ModeRemappingEntry {
                    final_refresh_rate: "119.95".into(),
                    ..ModeRemappingEntry::default()
                },
                ModeRemappingEntry {
                    requested_resolution: "1920x1080".into(),
                    requested_fps: "60".into(),
                    final_resolution: "2560x1440".into(),
                    final_refresh_rate: "120".into(),
                },
            ],
            ..ModeRemapping::default()
        };
        let requested = mode(1920, 1080, 60_000, 1_000);

        assert_eq!(
            remapping.resolve(context(true, true, true), requested),
            Ok(mode(1920, 1080, 11_995, 100))
        );
    }

    #[test]
    fn rational_matching_is_exact_and_accepts_equivalent_scales() {
        let remapping = ModeRemapping {
            refresh_rate_only: vec![ModeRemappingEntry {
                requested_fps: "60".into(),
                final_refresh_rate: "59.94".into(),
                ..ModeRemappingEntry::default()
            }],
            ..ModeRemapping::default()
        };

        assert_eq!(
            remapping.resolve(context(false, true, true), mode(1920, 1080, 60_000, 1_000)),
            Ok(mode(1920, 1080, 5_994, 100))
        );
        let nearly_sixty = mode(1920, 1080, 60_000, 1_001);
        assert_eq!(
            remapping.resolve(context(false, true, true), nearly_sixty),
            Ok(nearly_sixty)
        );
    }

    #[test]
    fn malformed_active_entry_and_missing_output_are_rejected() {
        let malformed = ModeRemapping {
            mixed: vec![ModeRemappingEntry {
                requested_resolution: "1920 by 1080".into(),
                final_resolution: "2560x1440".into(),
                ..ModeRemappingEntry::default()
            }],
            ..ModeRemapping::default()
        };
        assert_eq!(
            malformed.resolve(context(true, true, true), mode(1920, 1080, 60, 1)),
            Err(ModeRemappingError::InvalidField {
                entry_index: 0,
                field: "requested_resolution"
            })
        );

        let missing_output = ModeRemapping {
            mixed: vec![ModeRemappingEntry::default()],
            ..ModeRemapping::default()
        };
        assert_eq!(
            missing_output.resolve(context(true, true, true), mode(1920, 1080, 60, 1)),
            Err(ModeRemappingError::MissingFinalOutput { entry_index: 0 })
        );
    }

    #[test]
    fn client_override_bypasses_mapping_and_inactive_lists_are_not_parsed() {
        let remapping = ModeRemapping {
            mixed: vec![ModeRemappingEntry {
                requested_fps: "not a rate".into(),
                final_refresh_rate: "120".into(),
                ..ModeRemappingEntry::default()
            }],
            refresh_rate_only: vec![ModeRemappingEntry {
                requested_fps: "60".into(),
                final_refresh_rate: "120".into(),
                ..ModeRemappingEntry::default()
            }],
            ..ModeRemapping::default()
        };
        let requested = mode(1920, 1080, 60, 1);
        let override_context = ResolverContext {
            client_display_mode_override: true,
            ..context(true, true, true)
        };

        assert_eq!(
            remapping.resolve(override_context, requested),
            Ok(requested)
        );
        assert_eq!(
            remapping.resolve(context(false, true, true), requested),
            Ok(mode(1920, 1080, 120, 1))
        );
    }

    #[test]
    fn optimize_game_settings_gates_resolution_rules_but_keeps_rate_rules() {
        let remapping = ModeRemapping {
            mixed: vec![
                ModeRemappingEntry {
                    requested_resolution: "1920x1080".into(),
                    requested_fps: "60".into(),
                    final_resolution: "2560x1440".into(),
                    final_refresh_rate: "120".into(),
                },
                ModeRemappingEntry {
                    requested_fps: "60".into(),
                    final_refresh_rate: "90".into(),
                    ..ModeRemappingEntry::default()
                },
            ],
            ..ModeRemapping::default()
        };
        let requested = mode(1920, 1080, 60, 1);

        assert_eq!(
            remapping.resolve(context(true, true, false), requested),
            Ok(mode(1920, 1080, 90, 1))
        );
        assert_eq!(
            remapping.resolve(context(true, true, true), requested),
            Ok(mode(2560, 1440, 120, 1))
        );
    }

    #[test]
    fn malformed_resolution_rule_is_ignored_when_optimization_is_disabled() {
        let remapping = ModeRemapping {
            mixed: vec![
                ModeRemappingEntry {
                    requested_fps: "60".into(),
                    final_refresh_rate: "120".into(),
                    final_resolution: "invalid".into(),
                    ..ModeRemappingEntry::default()
                },
                ModeRemappingEntry {
                    requested_fps: "60".into(),
                    final_refresh_rate: "90".into(),
                    ..ModeRemappingEntry::default()
                },
            ],
            ..ModeRemapping::default()
        };
        let requested = mode(1920, 1080, 60, 1);

        assert_eq!(
            remapping.resolve(context(true, true, false), requested),
            Ok(mode(1920, 1080, 90, 1))
        );
    }
}
