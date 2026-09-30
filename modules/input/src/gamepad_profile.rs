use serde::{Deserialize, Serialize};

/// A platform's persisted controller profile choice.
///
/// The enum contains the choices exposed by Vibeshine on this target. Backends
/// may recognize a profile without implementing it; those explicit selections
/// are reported as unavailable and never fall back to `auto`.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GamepadProfile {
    #[default]
    Auto,
    #[cfg(windows)]
    X360,
    #[cfg(windows)]
    Vhf,
    #[cfg(windows)]
    VhfXbox,
    #[cfg(windows)]
    VhfXboxOne,
    #[cfg(windows)]
    VhfDs4,
    #[cfg(windows)]
    VhfDs5,
    #[cfg(windows)]
    VhfSwitch,
    #[cfg(any(windows, target_os = "linux"))]
    Ds4,
    #[cfg(target_os = "linux")]
    Xone,
    #[cfg(target_os = "linux")]
    Ds5,
    #[cfg(target_os = "linux")]
    Switch,
}

impl GamepadProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            #[cfg(windows)]
            Self::X360 => "x360",
            #[cfg(windows)]
            Self::Vhf => "vhf",
            #[cfg(windows)]
            Self::VhfXbox => "vhf_xbox",
            #[cfg(windows)]
            Self::VhfXboxOne => "vhf_xbox_one",
            #[cfg(windows)]
            Self::VhfDs4 => "vhf_ds4",
            #[cfg(windows)]
            Self::VhfDs5 => "vhf_ds5",
            #[cfg(windows)]
            Self::VhfSwitch => "vhf_switch",
            #[cfg(any(windows, target_os = "linux"))]
            Self::Ds4 => "ds4",
            #[cfg(target_os = "linux")]
            Self::Xone => "xone",
            #[cfg(target_os = "linux")]
            Self::Ds5 => "ds5",
            #[cfg(target_os = "linux")]
            Self::Switch => "switch",
        }
    }

    pub const fn implemented_profiles() -> &'static [&'static str] {
        #[cfg(windows)]
        {
            &["auto", "x360", "ds4"]
        }
        #[cfg(target_os = "linux")]
        {
            &["auto", "xone", "ds4", "ds5", "switch"]
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            &["auto"]
        }
    }

    /// Choices accepted from Vibeshine configuration on this target. This is
    /// broader than `implemented_profiles`: recognized unavailable choices
    /// must remain visible and must never silently fall back to `auto`.
    pub const fn recognized_profiles() -> &'static [&'static str] {
        #[cfg(windows)]
        {
            &[
                "auto",
                "x360",
                "vhf",
                "vhf_xbox",
                "vhf_xbox_one",
                "vhf_ds4",
                "vhf_ds5",
                "vhf_switch",
                "ds4",
            ]
        }
        #[cfg(target_os = "linux")]
        {
            &["auto", "xone", "ds4", "ds5", "switch"]
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            &["auto"]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recognized_profiles_round_trip_and_cover_implemented_profiles() {
        for &profile_token in GamepadProfile::recognized_profiles() {
            let profile = serde_json::from_value::<GamepadProfile>(json!(profile_token)).unwrap();

            assert_eq!(profile.as_str(), profile_token);
            assert_eq!(serde_json::to_value(profile).unwrap(), json!(profile_token));
        }

        for &implemented_token in GamepadProfile::implemented_profiles() {
            assert!(
                GamepadProfile::recognized_profiles().contains(&implemented_token),
                "implemented profile {implemented_token:?} is not recognized"
            );
        }
    }

    #[test]
    fn profile_validation_uses_pinned_choices_for_this_platform() {
        assert_eq!(
            serde_json::from_value::<GamepadProfile>(json!("auto")).unwrap(),
            GamepadProfile::Auto
        );
        assert!(serde_json::from_value::<GamepadProfile>(json!("not_a_profile")).is_err());

        #[cfg(windows)]
        for name in [
            "x360",
            "ds4",
            "vhf",
            "vhf_xbox",
            "vhf_xbox_one",
            "vhf_ds4",
            "vhf_ds5",
            "vhf_switch",
        ] {
            assert!(serde_json::from_value::<GamepadProfile>(json!(name)).is_ok());
        }
        #[cfg(windows)]
        assert!(serde_json::from_value::<GamepadProfile>(json!("xone")).is_err());

        #[cfg(target_os = "linux")]
        for name in ["xone", "ds4", "ds5", "switch"] {
            assert!(serde_json::from_value::<GamepadProfile>(json!(name)).is_ok());
        }
        #[cfg(target_os = "linux")]
        assert!(GamepadProfile::implemented_profiles().contains(&"ds5"));
        #[cfg(target_os = "linux")]
        assert!(GamepadProfile::implemented_profiles().contains(&"switch"));
        #[cfg(target_os = "linux")]
        assert!(serde_json::from_value::<GamepadProfile>(json!("vhf")).is_err());
    }
}
