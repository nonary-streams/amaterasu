//! Reusable typed configuration for resolution selection.

use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::ResolutionOption;

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
}
