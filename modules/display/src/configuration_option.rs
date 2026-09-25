use serde::{Deserialize, Serialize};

/// Describes how a configuration prepares the selected display.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayConfigurationOption {
    /// Require the selected display to be active without changing display state.
    #[default]
    VerifyOnly,
    /// Do not perform additional display configuration.
    Disabled,
    /// Activate the selected display if it is inactive.
    EnsureActive,
    /// Activate the selected display if it is inactive and make it primary.
    EnsurePrimary,
    /// Activate the selected display if it is inactive and disable other displays.
    EnsureOnlyDisplay,
}

impl DisplayConfigurationOption {
    /// Parses a legacy configuration token, falling back to Disabled if unknown.
    pub fn from_legacy_token(token: &str) -> Self {
        match token {
            "disabled" => Self::Disabled,
            "verify_only" => Self::VerifyOnly,
            "ensure_active" => Self::EnsureActive,
            "ensure_primary" => Self::EnsurePrimary,
            "ensure_only_display" => Self::EnsureOnlyDisplay,
            _ => Self::Disabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayConfigurationOption;

    #[test]
    fn defaults_to_verify_only_and_serializes_default_token() {
        assert_eq!(
            DisplayConfigurationOption::default(),
            DisplayConfigurationOption::VerifyOnly
        );
        assert_eq!(
            serde_json::to_string(&DisplayConfigurationOption::default()).unwrap(),
            r#""verify_only""#
        );
    }

    #[test]
    fn parses_exact_tokens_and_round_trips() {
        let options = [
            ("disabled", DisplayConfigurationOption::Disabled),
            ("verify_only", DisplayConfigurationOption::VerifyOnly),
            ("ensure_active", DisplayConfigurationOption::EnsureActive),
            ("ensure_primary", DisplayConfigurationOption::EnsurePrimary),
            (
                "ensure_only_display",
                DisplayConfigurationOption::EnsureOnlyDisplay,
            ),
        ];

        for (token, option) in options {
            let serialized = serde_json::to_string(&option).unwrap();
            assert_eq!(serialized, format!(r#""{token}""#));
            assert_eq!(
                serde_json::from_str::<DisplayConfigurationOption>(&serialized).unwrap(),
                option
            );
            assert_eq!(DisplayConfigurationOption::from_legacy_token(token), option);
        }
    }

    #[test]
    fn unknown_legacy_tokens_disable() {
        assert_eq!(
            DisplayConfigurationOption::from_legacy_token("future_option"),
            DisplayConfigurationOption::Disabled
        );
    }

    #[test]
    fn rejects_unknown_tokens_and_non_string_values_during_deserialization() {
        assert!(serde_json::from_str::<DisplayConfigurationOption>(r#""future_option""#).is_err());

        for value in ["null", "true", "42", "{}", "[]"] {
            assert!(
                serde_json::from_str::<DisplayConfigurationOption>(value).is_err(),
                "unexpectedly accepted {value}"
            );
        }
    }
}
