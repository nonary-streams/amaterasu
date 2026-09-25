use serde::{Deserialize, Serialize};

/// Describes how a configuration chooses a target display refresh rate.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshRateOption {
    /// Use the client's requested FPS as the target refresh rate.
    #[default]
    Auto,
    /// Do not perform additional refresh-rate configuration.
    Disabled,
    /// Use the companion `dd_manual_refresh_rate` setting.
    Manual,
    /// Prefer the highest refresh rate available for the selected resolution.
    PreferHighest,
}

impl RefreshRateOption {
    /// Parses a legacy configuration token, falling back to Disabled if unknown.
    pub fn from_legacy_token(token: &str) -> Self {
        match token {
            "auto" => Self::Auto,
            "disabled" => Self::Disabled,
            "manual" => Self::Manual,
            "prefer_highest" => Self::PreferHighest,
            _ => Self::Disabled,
        }
    }

    /// Returns whether the client FPS is active as the target refresh rate.
    pub const fn is_automatic(self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[cfg(test)]
mod tests {
    use super::RefreshRateOption;

    #[test]
    fn defaults_to_auto_and_serializes_default_token() {
        assert_eq!(RefreshRateOption::default(), RefreshRateOption::Auto);
        assert_eq!(
            serde_json::to_string(&RefreshRateOption::default()).unwrap(),
            "\"auto\""
        );
    }

    #[test]
    fn parses_and_serializes_exact_tokens() {
        let options = [
            ("auto", RefreshRateOption::Auto),
            ("disabled", RefreshRateOption::Disabled),
            ("manual", RefreshRateOption::Manual),
            ("prefer_highest", RefreshRateOption::PreferHighest),
        ];

        for (token, option) in options {
            assert_eq!(
                serde_json::from_str::<RefreshRateOption>(&format!("\"{token}\"")).unwrap(),
                option
            );
            assert_eq!(
                serde_json::to_string(&option).unwrap(),
                format!("\"{token}\"")
            );
            assert_eq!(RefreshRateOption::from_legacy_token(token), option);
        }
    }

    #[test]
    fn unknown_legacy_tokens_disable_and_automatic_is_only_auto() {
        assert_eq!(
            RefreshRateOption::from_legacy_token("future_option"),
            RefreshRateOption::Disabled
        );
        assert!(RefreshRateOption::Auto.is_automatic());
        for option in [
            RefreshRateOption::Disabled,
            RefreshRateOption::Manual,
            RefreshRateOption::PreferHighest,
        ] {
            assert!(!option.is_automatic());
        }
    }

    #[test]
    fn rejects_non_string_json_values() {
        for value in ["null", "true", "42", "{}", "[]"] {
            assert!(
                serde_json::from_str::<RefreshRateOption>(value).is_err(),
                "unexpectedly accepted {value}"
            );
        }
    }
}
