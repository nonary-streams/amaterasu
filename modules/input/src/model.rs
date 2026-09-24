use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_EVENTS: usize = 64;
pub const MAX_PRESSED: usize = 128;
pub const MAX_QUEUED_REQUESTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevicePolicy {
    pub keyboard: bool,
    pub mouse: bool,
}

impl Default for DevicePolicy {
    fn default() -> Self {
        Self {
            keyboard: true,
            mouse: true,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Acquire {
        #[serde(default)]
        operation_id: Option<String>,
        ttl_ms: Option<u64>,
    },
    Lookup {
        operation_id: String,
    },
    Renew {
        lease: String,
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
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum Key {
    WindowsScan {
        code: u16,
        #[serde(default)]
        extended: bool,
    },
    LinuxEvdev {
        code: u16,
    },
    MoonlightVk {
        code: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEncoding {
    WindowsScan,
    LinuxEvdev,
    MoonlightVk,
}

impl Key {
    pub fn encoding(&self) -> KeyEncoding {
        match self {
            Self::WindowsScan { .. } => KeyEncoding::WindowsScan,
            Self::LinuxEvdev { .. } => KeyEncoding::LinuxEvdev,
            Self::MoonlightVk { .. } => KeyEncoding::MoonlightVk,
        }
    }

    pub fn canonical(&self) -> Self {
        match self {
            Self::MoonlightVk { code: 0x10 } => Self::MoonlightVk { code: 0xa0 },
            Self::MoonlightVk { code: 0x11 } => Self::MoonlightVk { code: 0xa2 },
            Self::MoonlightVk { code: 0x12 } => Self::MoonlightVk { code: 0xa4 },
            key => key.clone(),
        }
    }
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
    ScrollUnits { axis: Axis, amount: i16 },
}
impl Event {
    pub fn enabled_by(&self, policy: DevicePolicy) -> bool {
        match self {
            Self::Key { .. } => policy.keyboard,
            Self::Button { .. }
            | Self::Relative { .. }
            | Self::Absolute { .. }
            | Self::Scroll { .. }
            | Self::ScrollUnits { .. } => policy.mouse,
        }
    }

    pub fn validate(&self) -> InputResult<()> {
        match self {
            Self::Key {
                key: Key::WindowsScan { code, .. },
                ..
            } if !(1..=0x7f).contains(code) => Err(InputError::new(
                "invalid_event",
                "Windows set-1 scan codes must be 1..127; E1 sequences are unsupported",
            )),
            Self::Key {
                key: Key::LinuxEvdev { code },
                ..
            } if !(1..=248).contains(code) || *code == 240 => Err(InputError::new(
                "invalid_event",
                "evdev keyboard codes must be 1..248 excluding KEY_UNKNOWN",
            )),
            Self::Key {
                key: Key::MoonlightVk { code },
                ..
            } if amaterasu_gamestream::control::virtual_key_mapping(*code).is_none() => {
                Err(InputError::new(
                    "invalid_event",
                    "unsupported normalized Moonlight virtual key",
                ))
            }
            Self::Relative { dx, dy } if dx.abs_diff(0) > 32_767 || dy.abs_diff(0) > 32_767 => {
                Err(InputError::new(
                    "invalid_event",
                    "relative mouse deltas must be -32767..32767",
                ))
            }
            Self::Scroll { steps, .. } if *steps == 0 || steps.abs_diff(0) > 100 => {
                Err(InputError::new(
                    "invalid_event",
                    "scroll steps must be nonzero and between -100 and 100",
                ))
            }
            Self::ScrollUnits { amount, .. } if *amount == 0 => Err(InputError::new(
                "invalid_event",
                "high-resolution scroll units must be nonzero",
            )),
            _ => Ok(()),
        }
    }
    pub fn transition(&self) -> Option<(Pressed, bool)> {
        match self {
            Self::Key {
                key,
                state: KeyState::Down,
            } => Some((Pressed::Key(key.canonical()), true)),
            Self::Key {
                key,
                state: KeyState::Up,
            } => Some((Pressed::Key(key.canonical()), false)),
            Self::Button { button, pressed } => Some((Pressed::Button(*button), *pressed)),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pressed {
    Key(Key),
    Button(Button),
}
impl Pressed {
    pub fn release_event(&self) -> Event {
        match self {
            Self::Key(key) => Event::Key {
                key: key.clone(),
                state: KeyState::Up,
            },
            Self::Button(button) => Event::Button {
                button: *button,
                pressed: false,
            },
        }
    }
}
#[derive(Clone, Debug)]
pub struct InputError {
    pub code: &'static str,
    pub message: String,
}
impl InputError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn payload(self) -> Value {
        json!({"ok":false,"error":{"code":self.code,"message":self.message.chars().take(512).collect::<String>()}})
    }
}
pub type InputResult<T> = Result<T, InputError>;
pub fn lease_duration(ttl_ms: Option<u64>) -> InputResult<u64> {
    let duration = ttl_ms.unwrap_or(5000);
    if !(1000..=30_000).contains(&duration) {
        return Err(InputError::new(
            "invalid_request",
            "lease duration must be 1000..30000 milliseconds",
        ));
    }
    Ok(duration)
}

pub fn validate_operation_id(operation_id: &str) -> InputResult<()> {
    if operation_id.is_empty()
        || operation_id.len() > 128
        || !operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:".contains(&byte))
    {
        return Err(InputError::new(
            "invalid_request",
            "operation_id must contain 1..128 identifier characters",
        ));
    }
    Ok(())
}
