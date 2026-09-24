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
        /// `true` preserves Moonlight's normalized fixed-US key form. Older
        /// clients omitted this field and therefore retain that behavior.
        #[serde(default = "default_true")]
        normalized: bool,
        /// Synthetic modifier bits captured for the repeating key-down that
        /// uses this key. Bits are Shift=1, Ctrl=2, Alt=4.
        #[serde(default, skip_serializing_if = "is_zero")]
        repeat_modifiers: u8,
        /// Marks a short-lived modifier synthesized around a Moonlight key
        /// event. This lets input validation distinguish it from a held key.
        #[serde(default, skip_serializing_if = "is_false")]
        temporary_modifier: bool,
    },
}

const fn default_true() -> bool {
    true
}

const fn is_zero(value: &u8) -> bool {
    *value == 0
}

const fn is_false(value: &bool) -> bool {
    !*value
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
            Self::MoonlightVk {
                code: 0x10,
                normalized,
                repeat_modifiers,
                temporary_modifier,
            } => Self::MoonlightVk {
                code: 0xa0,
                normalized: *normalized,
                repeat_modifiers: *repeat_modifiers,
                temporary_modifier: *temporary_modifier,
            },
            Self::MoonlightVk {
                code: 0x11,
                normalized,
                repeat_modifiers,
                temporary_modifier,
            } => Self::MoonlightVk {
                code: 0xa2,
                normalized: *normalized,
                repeat_modifiers: *repeat_modifiers,
                temporary_modifier: *temporary_modifier,
            },
            Self::MoonlightVk {
                code: 0x12,
                normalized,
                repeat_modifiers,
                temporary_modifier,
            } => Self::MoonlightVk {
                code: 0xa4,
                normalized: *normalized,
                repeat_modifiers: *repeat_modifiers,
                temporary_modifier: *temporary_modifier,
            },
            key => key.clone(),
        }
    }
}

pub(crate) const fn effective_virtual_key(code: u8, key_rightalt_to_key_win: bool) -> u8 {
    if key_rightalt_to_key_win && code == 0xa5 {
        0x5b
    } else {
        code
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
    Key {
        key: Key,
        state: KeyState,
    },
    Button {
        button: Button,
        pressed: bool,
    },
    Relative {
        dx: i32,
        dy: i32,
    },
    Absolute {
        x: u16,
        y: u16,
    },
    Scroll {
        axis: Axis,
        steps: i32,
    },
    ScrollUnits {
        axis: Axis,
        amount: i16,
    },
    Controller {
        number: u16,
        active_mask: u16,
        buttons: u32,
        left_trigger: u8,
        right_trigger: u8,
        left_x: i16,
        left_y: i16,
        right_x: i16,
        right_y: i16,
    },
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
            // Route controller records through the backend capability check;
            // unsupported native backends reject the batch before it is applied.
            Self::Controller { .. } => true,
        }
    }

    pub fn validate(&self) -> InputResult<()> {
        if let Self::Key { key, state } = self {
            validate_key_metadata(key, *state)?;
        }
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
                key:
                    Key::MoonlightVk {
                        code,
                        normalized: true,
                        ..
                    },
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
            Self::Controller { number, .. } if *number >= 16 => Err(InputError::new(
                "invalid_event",
                "controller number must be in the range 0..16",
            )),
            _ => Ok(()),
        }
    }
    pub fn transition(&self) -> Option<(Pressed, bool)> {
        match self {
            Self::Key {
                key,
                state: KeyState::Down,
            } => Some((Pressed::Key(key.clone()), true)),
            Self::Key {
                key,
                state: KeyState::Up,
            } => Some((Pressed::Key(key.clone()), false)),
            Self::Button { button, pressed } => Some((Pressed::Button(*button), *pressed)),
            _ => None,
        }
    }
}

fn validate_key_metadata(key: &Key, state: KeyState) -> InputResult<()> {
    let Key::MoonlightVk {
        code,
        repeat_modifiers,
        temporary_modifier,
        ..
    } = key
    else {
        return Ok(());
    };
    if repeat_modifiers & !0b111 != 0 {
        return Err(InputError::new(
            "invalid_event",
            "repeat modifier mask may contain only Shift, Ctrl, and Alt",
        ));
    }
    if *temporary_modifier {
        if !matches!(*code, 0x10..=0x12)
            || !matches!(state, KeyState::Down | KeyState::Up)
            || *repeat_modifiers != 0
        {
            return Err(InputError::new(
                "invalid_event",
                "temporary modifiers must be generic Shift, Ctrl, or Alt transitions",
            ));
        }
    } else if *repeat_modifiers != 0 {
        if state != KeyState::Down {
            return Err(InputError::new(
                "invalid_event",
                "repeat modifiers are valid only on a key-down event",
            ));
        }
        if is_modifier_virtual_key(*code) {
            return Err(InputError::new(
                "invalid_event",
                "a modifier key cannot request synthetic repeat modifiers",
            ));
        }
    }
    Ok(())
}

fn is_modifier_virtual_key(code: u8) -> bool {
    matches!(code, 0x10 | 0x11 | 0x12 | 0x5b | 0x5c | 0xa0..=0xa5)
}

pub(crate) fn repeat_modifiers(key: &Key) -> u8 {
    match key {
        Key::MoonlightVk {
            repeat_modifiers, ..
        } => *repeat_modifiers,
        _ => 0,
    }
}

pub(crate) fn is_temporary_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::MoonlightVk {
            temporary_modifier: true,
            ..
        }
    )
}
#[derive(Clone, Debug)]
pub enum Pressed {
    Key(Key),
    Button(Button),
}

impl PartialEq for Pressed {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Key(left), Self::Key(right)) => key_identity_eq(left, right),
            (Self::Button(left), Self::Button(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for Pressed {}

fn key_identity_eq(left: &Key, right: &Key) -> bool {
    match (left, right) {
        (
            Key::WindowsScan {
                code: left_code,
                extended: left_extended,
            },
            Key::WindowsScan {
                code: right_code,
                extended: right_extended,
            },
        ) => left_code == right_code && left_extended == right_extended,
        (Key::LinuxEvdev { code: left }, Key::LinuxEvdev { code: right }) => left == right,
        (Key::MoonlightVk { code: left, .. }, Key::MoonlightVk { code: right, .. }) => {
            canonical_virtual_key(*left) == canonical_virtual_key(*right)
        }
        _ => false,
    }
}

const fn canonical_virtual_key(code: u8) -> u8 {
    match code {
        0x10 => 0xa0,
        0x11 => 0xa2,
        0x12 => 0xa4,
        code => code,
    }
}

impl Pressed {
    pub fn release_event(&self) -> Event {
        match self {
            Self::Key(key) => Event::Key {
                key: key.without_repeat_modifiers(),
                state: KeyState::Up,
            },
            Self::Button(button) => Event::Button {
                button: *button,
                pressed: false,
            },
        }
    }
}

impl Key {
    pub(crate) fn clear_repeat_modifiers(&mut self) {
        if let Self::MoonlightVk {
            repeat_modifiers, ..
        } = self
        {
            *repeat_modifiers = 0;
        }
    }

    fn without_repeat_modifiers(&self) -> Self {
        match self {
            Self::MoonlightVk {
                code,
                normalized,
                temporary_modifier,
                ..
            } => Self::MoonlightVk {
                code: *code,
                normalized: *normalized,
                repeat_modifiers: 0,
                temporary_modifier: *temporary_modifier,
            },
            key => key.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_alt_remapping_uses_left_windows_virtual_key() {
        assert_eq!(effective_virtual_key(0xa5, true), 0x5b);
        assert_eq!(effective_virtual_key(0xa5, false), 0xa5);
        assert_eq!(effective_virtual_key(0xa4, true), 0xa4);
        let mapping =
            amaterasu_gamestream::control::virtual_key_mapping(effective_virtual_key(0xa5, true))
                .expect("left Windows key has a Linux mapping");
        assert_eq!(mapping.linux_evdev_code, 125); // KEY_LEFTMETA
    }

    #[test]
    fn moonlight_key_wire_defaults_to_normalized_and_accepts_raw_vk_mode() {
        let legacy: Event = serde_json::from_value(json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":65},
            "state":"down",
        }))
        .unwrap();
        assert_eq!(
            legacy,
            Event::Key {
                key: Key::MoonlightVk {
                    code: 65,
                    normalized: true,
                    repeat_modifiers: 0,
                    temporary_modifier: false,
                },
                state: KeyState::Down,
            }
        );

        let raw: Event = serde_json::from_value(json!({
            "kind":"key",
            "key":{"platform":"moonlight_vk","code":255,"normalized":false},
            "state":"down",
        }))
        .unwrap();
        assert!(raw.validate().is_ok());
        assert_eq!(
            raw,
            Event::Key {
                key: Key::MoonlightVk {
                    code: 255,
                    normalized: false,
                    repeat_modifiers: 0,
                    temporary_modifier: false,
                },
                state: KeyState::Down,
            }
        );
    }

    #[test]
    fn controller_event_round_trips_all_bounded_wire_fields() {
        let payload = json!({
            "kind":"controller",
            "number":15,
            "active_mask":0xa55a,
            "buttons":0x89abcdefu32,
            "left_trigger":0,
            "right_trigger":255,
            "left_x":-32768,
            "left_y":32767,
            "right_x":-1,
            "right_y":1,
        });
        let event: Event = serde_json::from_value(payload.clone()).unwrap();

        assert_eq!(
            event,
            Event::Controller {
                number: 15,
                active_mask: 0xa55a,
                buttons: 0x89abcdef,
                left_trigger: 0,
                right_trigger: 255,
                left_x: -32768,
                left_y: 32767,
                right_x: -1,
                right_y: 1,
            }
        );
        assert!(event.validate().is_ok());
        assert!(event.enabled_by(DevicePolicy::default()));
        assert_eq!(serde_json::to_value(event).unwrap(), payload);
    }

    #[test]
    fn controller_event_rejects_slots_outside_the_protocol_range() {
        let event: Event = serde_json::from_value(json!({
            "kind":"controller",
            "number":16,
            "active_mask":0,
            "buttons":0,
            "left_trigger":0,
            "right_trigger":0,
            "left_x":0,
            "left_y":0,
            "right_x":0,
            "right_y":0,
        }))
        .unwrap();

        assert_eq!(event.validate().unwrap_err().code, "invalid_event");
    }
}
