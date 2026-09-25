use serde::{Deserialize, Serialize};

/// A reusable choice for HDR configuration on a display.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayHdrOption {
    /// Let HDR selection follow the automatic policy.
    #[default]
    Auto,
    /// Prefer SDR output.
    Disabled,
}

impl DisplayHdrOption {
    /// Parse a legacy token, falling back to `Disabled` when it is not recognized.
    pub fn from_legacy_token(token: &str) -> Self {
        match token {
            "auto" => Self::Auto,
            "disabled" => Self::Disabled,
            _ => Self::Disabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayHdrOption;

    #[test]
    fn default_is_auto() {
        assert_eq!(DisplayHdrOption::default(), DisplayHdrOption::Auto);
    }

    #[test]
    fn serializes_exact_lowercase_tokens() {
        assert_eq!(
            serde_json::to_string(&DisplayHdrOption::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&DisplayHdrOption::Disabled).unwrap(),
            "\"disabled\""
        );
    }

    #[test]
    fn deserializes_exact_lowercase_tokens() {
        assert_eq!(
            serde_json::from_str::<DisplayHdrOption>("\"auto\"").unwrap(),
            DisplayHdrOption::Auto
        );
        assert_eq!(
            serde_json::from_str::<DisplayHdrOption>("\"disabled\"").unwrap(),
            DisplayHdrOption::Disabled
        );
    }

    #[test]
    fn serde_rejects_unknown_or_non_string_values() {
        for input in ["\"unknown\"", "\"AUTO\"", "null", "1", "{}", "[]"] {
            assert!(
                serde_json::from_str::<DisplayHdrOption>(input).is_err(),
                "expected {input} to be rejected"
            );
        }
    }

    #[test]
    fn legacy_parser_falls_back_to_disabled() {
        assert_eq!(
            DisplayHdrOption::from_legacy_token("auto"),
            DisplayHdrOption::Auto
        );
        assert_eq!(
            DisplayHdrOption::from_legacy_token("disabled"),
            DisplayHdrOption::Disabled
        );
        for token in ["", "unknown", "AUTO"] {
            assert_eq!(
                DisplayHdrOption::from_legacy_token(token),
                DisplayHdrOption::Disabled
            );
        }
    }
}
