//! Reusable typed configuration for resolution selection.

use crate::mode_remapping::Resolution;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Describes how a configuration chooses a target display resolution.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionOption {
    /// Change to the resolution requested by the client.
    #[default]
    Auto,
    /// Do not configure an additional resolution.
    Disabled,
    /// Use the companion dd_manual_resolution setting.
    Manual,
}

impl ResolutionOption {
    /// Parses a legacy configuration token, falling back to Disabled if unknown.
    pub fn from_legacy_token(token: &str) -> Self {
        match token {
            "auto" => Self::Auto,
            "disabled" => Self::Disabled,
            "manual" => Self::Manual,
            _ => Self::Disabled,
        }
    }

    /// Returns whether resolution selection is automatic.
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// A parsed value for the companion `dd_manual_resolution` setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualResolution {
    /// Leave the configured display resolution unspecified.
    Unspecified,
    /// Use the explicitly configured width and height.
    Specified(Resolution),
}

impl ManualResolution {
    /// Parses an optional `WIDTHxHEIGHT` resolution using ASCII decimal digits.
    ///
    /// Surrounding whitespace is ignored, and the multiplication sign `×` is
    /// accepted as a separator. Zero dimensions are preserved for callers to
    /// interpret.
    pub fn parse(value: &str) -> Result<Self, ManualResolutionParseError> {
        let normalized_value = value.trim().replace('×', "x");
        if normalized_value.is_empty() {
            return Ok(Self::Unspecified);
        }

        let Some((width_text, height_text)) = normalized_value.split_once('x') else {
            return Err(ManualResolutionParseError::InvalidFormat);
        };
        if width_text.is_empty()
            || height_text.is_empty()
            || height_text.contains('x')
            || !width_text.bytes().all(|byte| byte.is_ascii_digit())
            || !height_text.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(ManualResolutionParseError::InvalidFormat);
        }

        let width = width_text
            .parse::<u32>()
            .map_err(|_| ManualResolutionParseError::WidthOverflow)?;
        let height = height_text
            .parse::<u32>()
            .map_err(|_| ManualResolutionParseError::HeightOverflow)?;

        Ok(Self::Specified(Resolution { width, height }))
    }
}

/// Why a nonempty manual resolution setting could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualResolutionParseError {
    /// The value was not exactly two ASCII-decimal dimensions separated by `x`.
    InvalidFormat,
    /// The width was larger than `u32::MAX`.
    WidthOverflow,
    /// The height was larger than `u32::MAX`.
    HeightOverflow,
}

impl fmt::Display for ManualResolutionParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => write!(formatter, "expected WIDTHxHEIGHT using ASCII digits"),
            Self::WidthOverflow => write!(formatter, "manual resolution width exceeds u32::MAX"),
            Self::HeightOverflow => {
                write!(formatter, "manual resolution height exceeds u32::MAX")
            }
        }
    }
}

impl std::error::Error for ManualResolutionParseError {}

#[cfg(test)]
mod tests {
    use super::{ManualResolution, ManualResolutionParseError, ResolutionOption};
    use crate::mode_remapping::Resolution;

    #[test]
    fn defaults_to_auto_and_serializes_default_token() {
        assert_eq!(ResolutionOption::default(), ResolutionOption::Auto);
        assert_eq!(
            serde_json::to_string(&ResolutionOption::default()).unwrap(),
            r#""auto""#
        );
    }

    #[test]
    fn parses_exact_tokens_and_round_trips() {
        let options = [
            ("auto", ResolutionOption::Auto),
            ("disabled", ResolutionOption::Disabled),
            ("manual", ResolutionOption::Manual),
        ];

        for (token, option) in options {
            let serialized = serde_json::to_string(&option).unwrap();
            assert_eq!(serialized, format!(r#""{token}""#));
            assert_eq!(
                serde_json::from_str::<ResolutionOption>(&serialized).unwrap(),
                option
            );
            assert_eq!(ResolutionOption::from_legacy_token(token), option);
        }
    }

    #[test]
    fn unknown_legacy_tokens_disable() {
        assert_eq!(
            ResolutionOption::from_legacy_token("future_option"),
            ResolutionOption::Disabled
        );
    }

    #[test]
    fn rejects_unknown_tokens_during_deserialization() {
        assert!(serde_json::from_str::<ResolutionOption>(r#""future_option""#).is_err());
    }

    #[test]
    fn rejects_non_string_json_values() {
        for value in ["null", "true", "42", "{}", "[]"] {
            assert!(
                serde_json::from_str::<ResolutionOption>(value).is_err(),
                "unexpectedly accepted {value}"
            );
        }
    }

    #[test]
    fn is_automatic_only_for_auto() {
        assert!(ResolutionOption::Auto.is_automatic());
        assert!(!ResolutionOption::Disabled.is_automatic());
        assert!(!ResolutionOption::Manual.is_automatic());
    }

    #[test]
    fn manual_resolution_parses_blank_values_as_unspecified() {
        assert_eq!(
            ManualResolution::parse("").unwrap(),
            ManualResolution::Unspecified
        );
        assert_eq!(
            ManualResolution::parse(" \t\n ").unwrap(),
            ManualResolution::Unspecified
        );
    }

    #[test]
    fn manual_resolution_parses_ascii_and_multiplication_sign_separators() {
        let expected = ManualResolution::Specified(Resolution {
            width: 1920,
            height: 1080,
        });
        assert_eq!(ManualResolution::parse(" 1920x1080 ").unwrap(), expected);
        assert_eq!(ManualResolution::parse("1920×1080").unwrap(), expected);
    }

    #[test]
    fn manual_resolution_rejects_malformed_and_extra_components() {
        for value in [
            "x1080",
            "1920x",
            "1920",
            "1920x1080x60",
            "1920 x1080",
            "١x2",
        ] {
            assert_eq!(
                ManualResolution::parse(value),
                Err(ManualResolutionParseError::InvalidFormat),
                "unexpected parse result for {value:?}"
            );
        }
    }

    #[test]
    fn manual_resolution_reports_dimension_overflow() {
        assert_eq!(
            ManualResolution::parse("4294967296x1080"),
            Err(ManualResolutionParseError::WidthOverflow)
        );
        assert_eq!(
            ManualResolution::parse("1920x4294967296"),
            Err(ManualResolutionParseError::HeightOverflow)
        );
    }

    #[test]
    fn manual_resolution_accepts_zero_dimensions() {
        assert_eq!(
            ManualResolution::parse("0x0").unwrap(),
            ManualResolution::Specified(Resolution {
                width: 0,
                height: 0,
            })
        );
    }
}
