use amaterasu_input_policy::KeyBindings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_BATCH_EVENTS: usize = 64;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default = "default_proton_dualsense_compatibility")]
    pub proton_dualsense_compatibility: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            proton_dualsense_compatibility: true,
        }
    }
}

const fn default_proton_dualsense_compatibility() -> bool {
    true
}

impl Settings {
    pub fn settings_get_response(&self) -> Value {
        json!({
            "ok":true,
            "persisted":true,
            "applied":true,
            "cleanup_required":false,
            "revision":0,
            "values":{"proton_dualsense_compatibility":self.proton_dualsense_compatibility}
        })
    }
}

#[cfg(test)]
mod settings_tests {
    use super::Settings;
    use serde_json::json;

    #[test]
    fn settings_read_reports_persisted_default_proton_policy() {
        let settings: Settings = serde_json::from_value(json!({"enabled":true})).unwrap();
        let response = settings.settings_get_response();

        assert_eq!(response["ok"], true);
        assert_eq!(response["persisted"], true);
        assert_eq!(response["values"]["proton_dualsense_compatibility"], true);
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Acquire {
        #[serde(default)]
        operation_id: Option<String>,
        #[serde(default)]
        ttl_ms: Option<u64>,
        #[serde(default)]
        back_button_timeout: Option<i32>,
        #[serde(default)]
        controller: Option<bool>,
        #[serde(default)]
        gamepad: Option<String>,
        #[serde(default)]
        ds4_back_as_touchpad_click: Option<bool>,
        #[serde(default)]
        native_pen_touch: Option<bool>,
        #[serde(default)]
        keyboard: Option<bool>,
        #[serde(default)]
        mouse: Option<bool>,
        #[serde(default)]
        high_resolution_scrolling: Option<bool>,
        #[serde(default)]
        always_send_scancodes: Option<bool>,
        #[serde(default)]
        key_rightalt_to_key_win: Option<bool>,
        #[serde(default)]
        keybindings: Option<KeyBindings>,
        #[serde(default)]
        key_repeat_delay: Option<u32>,
        #[serde(default)]
        key_repeat_frequency: Option<f64>,
    },
    Lookup {
        operation_id: String,
    },
    Renew {
        lease: String,
        #[serde(default)]
        ttl_ms: Option<u64>,
    },
    Batch {
        lease: String,
        events: Vec<Event>,
    },
    Release {
        lease: String,
    },
}

/// Requested lease policy recorded by this synthetic provider. This is evidence
/// of contract transport only; no operating-system behavior is emulated.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RequestedPolicy {
    pub back_button_timeout: Option<i32>,
    pub controller: Option<bool>,
    pub gamepad: Option<String>,
    pub ds4_back_as_touchpad_click: Option<bool>,
    pub native_pen_touch: Option<bool>,
    pub keyboard: Option<bool>,
    pub mouse: Option<bool>,
    pub high_resolution_scrolling: Option<bool>,
    pub always_send_scancodes: Option<bool>,
    pub key_rightalt_to_key_win: Option<bool>,
    pub keybindings: Option<KeyBindings>,
    pub key_repeat_delay: Option<u32>,
    pub key_repeat_frequency: Option<f64>,
}

impl RequestedPolicy {
    pub fn validate(&self) -> FixtureResult<()> {
        if self
            .gamepad
            .as_deref()
            .is_some_and(|profile| !valid_gamepad_profile(profile))
        {
            return Err(FixtureError::new(
                "invalid_request",
                "invalid synthetic input gamepad profile",
            ));
        }
        if self
            .key_repeat_delay
            .is_some_and(|delay| delay > i32::MAX as u32)
            || self
                .key_repeat_frequency
                .is_some_and(|frequency| !frequency.is_finite() || frequency < 0.0)
        {
            return Err(FixtureError::new(
                "invalid_request",
                "invalid synthetic input repeat policy",
            ));
        }
        Ok(())
    }
}

// Match the input contract's target-specific recognized choices. This fixture
// records the choice without asserting native availability for any profile.
fn valid_gamepad_profile(profile: &str) -> bool {
    #[cfg(windows)]
    {
        matches!(
            profile,
            "auto"
                | "x360"
                | "vhf"
                | "vhf_xbox"
                | "vhf_xbox_one"
                | "vhf_ds4"
                | "vhf_ds5"
                | "vhf_switch"
                | "ds4"
        )
    }
    #[cfg(target_os = "linux")]
    {
        matches!(profile, "auto" | "xone" | "ds4" | "ds5" | "switch")
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        profile == "auto"
    }
}

impl Operation {
    pub fn mutates(&self) -> bool {
        !matches!(self, Self::Status | Self::Lookup { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum Key {
    MoonlightVk { code: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    Down,
    Up,
    Repeat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Key { key: Key, state: KeyState },
    Button { button: Button, pressed: bool },
    Relative { dx: i32, dy: i32 },
    Absolute { x: u16, y: u16 },
    Scroll { axis: Axis, steps: i32 },
}

impl Event {
    pub fn validate(&self) -> FixtureResult<()> {
        match self {
            Self::Key {
                key: Key::MoonlightVk { code },
                ..
            } if !(1..=255).contains(code) => Err(FixtureError::new(
                "invalid_event",
                "Moonlight virtual-key codes must be 1..255",
            )),
            Self::Relative { dx, dy } if dx.abs_diff(0) > 32_767 || dy.abs_diff(0) > 32_767 => {
                Err(FixtureError::new(
                    "invalid_event",
                    "relative mouse deltas must be -32767..32767",
                ))
            }
            Self::Scroll { steps, .. } if *steps == 0 || steps.abs_diff(0) > 100 => {
                Err(FixtureError::new(
                    "invalid_event",
                    "scroll steps must be nonzero and between -100 and 100",
                ))
            }
            _ => Ok(()),
        }
    }

    pub fn transition(&self) -> Option<(HeldInput, bool)> {
        match self {
            Self::Key {
                key,
                state: KeyState::Down,
            } => Some((HeldInput::Key(key.clone()), true)),
            Self::Key {
                key,
                state: KeyState::Up,
            } => Some((HeldInput::Key(key.clone()), false)),
            Self::Button { button, pressed } => Some((HeldInput::Button(*button), *pressed)),
            _ => None,
        }
    }

    pub fn repeated_key(&self) -> Option<&Key> {
        match self {
            Self::Key {
                key,
                state: KeyState::Repeat,
            } => Some(key),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeldInput {
    Key(Key),
    Button(Button),
}

#[derive(Clone, Debug)]
pub struct FixtureError {
    pub code: &'static str,
    pub message: String,
}

impl FixtureError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn payload(self) -> Value {
        json!({
            "ok": false,
            "error": {
                "code": self.code,
                "message": self.message.chars().take(512).collect::<String>(),
            },
            "cleanup_required": false,
        })
    }
}

pub type FixtureResult<T> = Result<T, FixtureError>;

pub fn lease_duration(ttl_ms: Option<u64>) -> FixtureResult<u64> {
    let duration = ttl_ms.unwrap_or(5000);
    if !(1000..=30_000).contains(&duration) {
        return Err(FixtureError::new(
            "invalid_request",
            "lease duration must be 1000..30000 milliseconds",
        ));
    }
    Ok(duration)
}

pub fn validate_operation_id(operation_id: &str) -> FixtureResult<()> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || operation_id.chars().any(char::is_control)
    {
        return Err(FixtureError::new(
            "invalid_operation_id",
            "operation_id must contain 1..128 non-control characters",
        ));
    }
    Ok(())
}
