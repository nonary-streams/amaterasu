use serde::{Deserialize, Serialize};

/// A reusable choice for HDR configuration on a display.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayHdrOption {
    /// Set display HDR to the client's requested state when the display supports it.
    #[default]
    Auto,
    /// Perform no additional display HDR configuration.
    Disabled,
}

/// Resolve an HDR option to the display HDR state selected for a provider.
///
/// The effective client HDR request must already be computed by the caller.
/// Higher-priority forced states, such as RTX HDR forced-SDR and
/// `dd_wa_dummy_plug_hdr10` forced-HDR, remain outside this helper. This pure
/// helper only selects a state; a provider applies it only when supported.
pub fn resolve_hdr_configuration(
    option: DisplayHdrOption,
    effective_client_hdr_request: bool,
) -> Option<bool> {
    match option {
        DisplayHdrOption::Auto => Some(effective_client_hdr_request),
        DisplayHdrOption::Disabled => None,
    }
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
    use super::{DisplayHdrOption, resolve_hdr_configuration};

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

    #[test]
    fn auto_resolves_true_client_request_to_hdr_enabled() {
        assert_eq!(
            resolve_hdr_configuration(DisplayHdrOption::Auto, true),
            Some(true)
        );
    }

    #[test]
    fn auto_resolves_false_client_request_to_hdr_disabled() {
        assert_eq!(
            resolve_hdr_configuration(DisplayHdrOption::Auto, false),
            Some(false)
        );
    }

    #[test]
    fn disabled_ignores_true_client_request() {
        assert_eq!(
            resolve_hdr_configuration(DisplayHdrOption::Disabled, true),
            None
        );
    }

    #[test]
    fn disabled_ignores_false_client_request() {
        assert_eq!(
            resolve_hdr_configuration(DisplayHdrOption::Disabled, false),
            None
        );
    }
}
