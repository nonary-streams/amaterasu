use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_BATCH_EVENTS: usize = 64;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
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
