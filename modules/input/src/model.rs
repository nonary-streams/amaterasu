use crate::gamepad_profile::GamepadProfile;
use amaterasu_input_policy::KeyBindings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_EVENTS: usize = 64;
pub const MAX_PRESSED: usize = 128;
pub const MAX_PEN_TOUCH_CONTACTS: usize = 16;
pub const MAX_QUEUED_REQUESTS: usize = 128;
/// Pen packets do not carry a pointer ID, so the ingress maps them to this
/// stable identity in the separate `Pen` device namespace.
pub const PEN_POINTER_ID: u32 = 1;
pub const PEN_BUTTON_PRIMARY: u8 = 1 << 0;
pub const PEN_BUTTON_SECONDARY: u8 = 1 << 1;
pub const PEN_BUTTON_TERTIARY: u8 = 1 << 2;
pub const PEN_BUTTON_MASK: u8 = PEN_BUTTON_PRIMARY | PEN_BUTTON_SECONDARY | PEN_BUTTON_TERTIARY;

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
        #[serde(default)]
        back_button_timeout: Option<i32>,
        #[serde(default)]
        controller: Option<bool>,
        #[serde(default)]
        gamepad: Option<GamepadProfile>,
        #[serde(default)]
        ds4_back_as_touchpad_click: Option<bool>,
        /// Enables delivery through the active backend. This is an event gate,
        /// not a guarantee of native pen/touch support; some backends map a
        /// limited touch subset to mouse input.
        #[serde(default)]
        motion_as_ds4: Option<bool>,
        #[serde(default)]
        touchpad_as_ds4: Option<bool>,
        #[serde(default)]
        ds5_inputtino_randomize_mac: Option<bool>,
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
        ttl_ms: Option<u64>,
    },
    Batch {
        lease: String,
        events: Vec<Event>,
    },
    FeedbackPeek {
        lease: String,
        #[serde(default = "default_feedback_limit")]
        limit: u8,
    },
    FeedbackAck {
        lease: String,
        through_sequence: u64,
    },
    PollGamepadFeedback {
        lease: String,
    },
    Release {
        lease: String,
    },
}

fn default_feedback_limit() -> u8 {
    16
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PenTouchDevice {
    Pen,
    Touch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PenTouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerMotionType {
    Accel,
    Gyro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerBatteryState {
    Unknown,
    NotPresent,
    Charging,
    Discharging,
    NotCharging,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerTouchPhase {
    Hover,
    Down,
    Up,
    Move,
    Cancel,
    ButtonOnly,
    HoverLeave,
    CancelAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PenTool {
    Unknown,
    Pen,
    Eraser,
}

/// Converts the protocol's polar tilt and clockwise rotation to native axis
/// tilt angles. Unknown or out-of-range samples produce no derived axes.
pub fn pen_tilt_axes(
    tilt_degrees: Option<u8>,
    rotation_degrees: Option<u16>,
) -> Option<(f32, f32)> {
    let (Some(tilt_degrees), Some(rotation_degrees)) = (tilt_degrees, rotation_degrees) else {
        return None;
    };
    if tilt_degrees > 90 || rotation_degrees > 359 {
        return None;
    }

    let rotation_radians = f32::from(rotation_degrees).to_radians();
    let tilt_radians = f32::from(tilt_degrees).to_radians();
    let radial = tilt_radians.sin();
    let vertical = tilt_radians.cos();
    Some((
        ((-rotation_radians).sin() * radial)
            .atan2(vertical)
            .to_degrees(),
        ((-rotation_radians).cos() * radial)
            .atan2(vertical)
            .to_degrees(),
    ))
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
    PenTouch {
        pointer_id: u32,
        device: PenTouchDevice,
        phase: PenTouchPhase,
        x: u16,
        y: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressure: Option<f32>,
        /// Touchport-transformed desktop length normalized by 65535.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contact_area_major: Option<f32>,
        /// Touchport-transformed desktop length normalized by 65535.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contact_area_minor: Option<f32>,
        /// None means rotation was not reported; known values are 0..359.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rotation_degrees: Option<u16>,
        /// Pen-only tool metadata. `Unknown` preserves an unrecognized tool.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pen_tool: Option<PenTool>,
        /// Pen-only primary/secondary/tertiary button bit mask.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pen_buttons: Option<u8>,
        /// Raw polar tilt magnitude in degrees from perpendicular (0..90).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tilt: Option<u8>,
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
    ControllerArrival {
        number: u16,
        controller_type: u8,
        capabilities: u32,
        supported_buttons: u32,
    },
    ControllerTouch {
        number: u16,
        touchpad_index: u8,
        pointer_id: u32,
        phase: ControllerTouchPhase,
        x: f32,
        y: f32,
        pressure: f32,
    },
    ControllerMotion {
        number: u16,
        motion_type: ControllerMotionType,
        x: f32,
        y: f32,
        z: f32,
    },
    ControllerBattery {
        number: u16,
        state: ControllerBatteryState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        percentage: Option<u8>,
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
            Self::PenTouch { .. } => true,
            // Route controller records through the backend capability check;
            // unsupported native backends reject the batch before it is applied.
            Self::Controller { .. }
            | Self::ControllerArrival { .. }
            | Self::ControllerTouch { .. }
            | Self::ControllerMotion { .. }
            | Self::ControllerBattery { .. } => true,
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
            Self::ControllerArrival { number, .. }
            | Self::ControllerTouch { number, .. }
            | Self::ControllerMotion { number, .. }
            | Self::ControllerBattery { number, .. }
                if *number >= 16 =>
            {
                Err(InputError::new(
                    "invalid_event",
                    "controller number must be in the range 0..16",
                ))
            }
            Self::ControllerTouch { touchpad_index, .. } if *touchpad_index > 1 => Err(
                InputError::new("invalid_event", "controller touchpad index must be 0 or 1"),
            ),
            Self::ControllerTouch { x, y, pressure, .. }
                if [*x, *y, *pressure]
                    .into_iter()
                    .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) =>
            {
                Err(InputError::new(
                    "invalid_event",
                    "controller touch coordinates and pressure must be finite and normalized to 0..1",
                ))
            }
            Self::ControllerMotion { x, y, z, .. }
                if [*x, *y, *z].into_iter().any(|value| !value.is_finite()) =>
            {
                Err(InputError::new(
                    "invalid_event",
                    "controller motion components must be finite",
                ))
            }
            Self::ControllerBattery {
                percentage: Some(percentage),
                ..
            } if *percentage > 100 => Err(InputError::new(
                "invalid_event",
                "controller battery percentage must be in the range 0..100",
            )),
            Self::PenTouch { .. } => validate_pen_touch(self),
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

fn validate_pen_touch(event: &Event) -> InputResult<()> {
    let Event::PenTouch {
        pointer_id,
        device,
        pressure,
        contact_area_major,
        contact_area_minor,
        rotation_degrees,
        pen_tool,
        pen_buttons,
        tilt,
        ..
    } = event
    else {
        unreachable!("pen/touch validation is called only for pen/touch events")
    };
    if *device == PenTouchDevice::Pen && *pointer_id != PEN_POINTER_ID {
        return Err(InputError::new(
            "invalid_event",
            "pen pointer_id must use the stable pen identity",
        ));
    }
    if (*pressure).is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
        return Err(InputError::new(
            "invalid_event",
            "pen/touch pressure must be finite and normalized to 0..1",
        ));
    }
    if contact_area_major.is_some() != contact_area_minor.is_some() {
        return Err(InputError::new(
            "invalid_event",
            "contact area major and minor must be provided together",
        ));
    }
    if [*contact_area_major, *contact_area_minor]
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(InputError::new(
            "invalid_event",
            "pen/touch contact area must be finite and normalized to 0..1",
        ));
    }
    if (*rotation_degrees).is_some_and(|rotation| rotation > 359) {
        return Err(InputError::new(
            "invalid_event",
            "pen/touch rotation must be in the range 0..359 degrees",
        ));
    }
    if (*pen_buttons).is_some_and(|buttons| buttons & !PEN_BUTTON_MASK != 0) {
        return Err(InputError::new(
            "invalid_event",
            "pen button mask may contain only primary, secondary, and tertiary bits",
        ));
    }
    if (*tilt).is_some_and(|tilt| tilt > 90) {
        return Err(InputError::new(
            "invalid_event",
            "pen tilt must be in the range 0..90 degrees",
        ));
    }
    if *device == PenTouchDevice::Touch
        && (pen_tool.is_some() || pen_buttons.is_some() || tilt.is_some())
    {
        return Err(InputError::new(
            "invalid_event",
            "pen tool, buttons, and tilt are supported only for pen events",
        ));
    }
    Ok(())
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
            left == right
        }
        _ => false,
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

/// Apply the legacy right-Alt mapping for companion wire translation.
pub(crate) const fn effective_virtual_key(code: u8, enabled: bool) -> u8 {
    if enabled && code == 0xa5 { 0x5b } else { code }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_keybindings_preserve_optional_ordered_byte_lists() {
        for supplied in [
            None,
            Some(Value::Null),
            Some(json!([])),
            Some(json!([0, 255, 65, 66, 65, 67])),
        ] {
            let mut request = json!({"action":"acquire"});
            if let Some(value) = supplied.clone() {
                request["keybindings"] = value;
            }
            let operation: Operation = serde_json::from_value(request).unwrap();
            let Operation::Acquire { keybindings, .. } = operation else {
                panic!("wrong operation");
            };
            assert_eq!(
                serde_json::to_value(keybindings).unwrap(),
                supplied.unwrap_or(Value::Null)
            );
        }
        for invalid in [
            json!([0]),
            json!([0, 256]),
            json!([-1, 0]),
            json!([0.0, 1]),
            json!(vec![0_u8; 514]),
        ] {
            assert!(
                serde_json::from_value::<Operation>(
                    json!({"action":"acquire", "keybindings":invalid})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn acquire_timeout_override_is_sparse_signed_and_strictly_bounded_by_i32() {
        let inherited: Operation = serde_json::from_value(json!({
            "action":"acquire",
            "operation_id":"stream-one",
            "ttl_ms":1000
        }))
        .unwrap();
        assert!(matches!(
            inherited,
            Operation::Acquire {
                back_button_timeout: None,
                native_pen_touch: None,
                ..
            }
        ));

        let disabled: Operation = serde_json::from_value(json!({
            "action":"acquire",
            "operation_id":"stream-two",
            "back_button_timeout":-1
        }))
        .unwrap();
        assert!(matches!(
            disabled,
            Operation::Acquire {
                back_button_timeout: Some(-1),
                ..
            }
        ));
        for value in [
            json!(2147483648i64),
            json!(-2147483649i64),
            json!(1.5),
            json!("50"),
        ] {
            let mut payload = json!({"action":"acquire"});
            payload["back_button_timeout"] = value;
            assert!(serde_json::from_value::<Operation>(payload).is_err());
        }
    }

    #[test]
    fn feedback_pull_actions_require_bounded_peek_and_u64_ack_sequence() {
        let peek: Operation = serde_json::from_value(json!({
            "action":"feedback_peek",
            "lease":"lease-token"
        }))
        .unwrap();
        assert!(matches!(peek, Operation::FeedbackPeek { limit: 16, .. }));

        let out_of_range_limit: Operation = serde_json::from_value(json!({
            "action":"feedback_peek",
            "lease":"x",
            "limit":17
        }))
        .unwrap();
        assert!(matches!(
            out_of_range_limit,
            Operation::FeedbackPeek { limit: 17, .. }
        ));

        for invalid in [
            json!({"action":"feedback_peek","lease":"x","limit":1.5}),
            json!({"action":"feedback_peek","lease":"x","extra":true}),
            json!({"action":"feedback_ack","lease":"x","through_sequence":-1}),
            json!({"action":"feedback_ack","lease":"x","through_sequence":1,"extra":true}),
        ] {
            let operation = serde_json::from_value::<Operation>(invalid);
            if let Ok(Operation::FeedbackPeek { limit, .. }) = &operation {
                assert!((1..=16).contains(limit));
            } else {
                assert!(operation.is_err());
            }
        }
    }

    #[test]
    fn acquire_accepts_sparse_input_policy_values_and_rejects_unknown_profiles() {
        let scoped: Operation = serde_json::from_value(json!({
            "action":"acquire",
            "controller":false,
            "gamepad":"auto",
            "keyboard":true,
            "mouse":false,
            "high_resolution_scrolling":false,
            "always_send_scancodes":false,
            "key_rightalt_to_key_win":true,
            "ds4_back_as_touchpad_click":false,
            "motion_as_ds4":true,
            "touchpad_as_ds4":true,
            "native_pen_touch":false,
            "key_repeat_delay":0,
            "key_repeat_frequency":0.0
        }))
        .unwrap();
        assert!(matches!(
            scoped,
            Operation::Acquire {
                controller: Some(false),
                gamepad: Some(GamepadProfile::Auto),
                ds4_back_as_touchpad_click: Some(false),
                native_pen_touch: Some(false),
                keyboard: Some(true),
                mouse: Some(false),
                high_resolution_scrolling: Some(false),
                always_send_scancodes: Some(false),
                key_rightalt_to_key_win: Some(true),
                key_repeat_delay: Some(0),
                key_repeat_frequency: Some(0.0),
                ..
            }
        ));
        for profile in ["not-a-profile", "xone"] {
            #[cfg(windows)]
            if profile == "xone" {
                assert!(
                    serde_json::from_value::<Operation>(json!({
                        "action":"acquire", "gamepad":profile
                    }))
                    .is_err()
                );
            }
            #[cfg(target_os = "linux")]
            if profile == "not-a-profile" {
                assert!(
                    serde_json::from_value::<Operation>(json!({
                        "action":"acquire", "gamepad":profile
                    }))
                    .is_err()
                );
            }
            #[cfg(not(any(windows, target_os = "linux")))]
            {
                assert!(
                    serde_json::from_value::<Operation>(json!({
                        "action":"acquire", "gamepad":profile
                    }))
                    .is_err()
                );
            }
        }

        assert!(
            serde_json::from_value::<Operation>(json!({
                "action":"acquire",
                "ds4_back_as_touchpad_click":"false"
            }))
            .is_err()
        );
    }

    #[test]
    fn acquire_native_pen_touch_is_an_optional_boolean() {
        for (value, expected) in [
            (None, None),
            (Some(true), Some(true)),
            (Some(false), Some(false)),
        ] {
            let mut payload = json!({"action":"acquire"});
            if let Some(value) = value {
                payload["native_pen_touch"] = json!(value);
            }
            let operation: Operation = serde_json::from_value(payload).unwrap();
            assert!(matches!(
                operation,
                Operation::Acquire {
                    native_pen_touch: actual,
                    ..
                } if actual == expected
            ));
        }

        assert!(
            serde_json::from_value::<Operation>(json!({
                "action":"acquire",
                "native_pen_touch":"true"
            }))
            .is_err()
        );
    }

    #[test]
    fn right_alt_remapping_uses_left_windows_virtual_key() {
        assert_eq!(
            amaterasu_input_policy::KeyBindings::default().resolve(0xa5, true),
            0x5b
        );
        assert_eq!(
            amaterasu_input_policy::KeyBindings::default().resolve(0xa5, false),
            0xa5
        );
        assert_eq!(
            amaterasu_input_policy::KeyBindings::default().resolve(0xa4, true),
            0xa4
        );
        let mapping = amaterasu_gamestream::control::virtual_key_mapping(
            amaterasu_input_policy::KeyBindings::default().resolve(0xa5, true),
        )
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

    #[test]
    fn controller_auxiliary_events_preserve_arrival_touch_motion_and_battery_fields() {
        let arrival = json!({
            "kind":"controller_arrival",
            "number":2,
            "controller_type":3,
            "capabilities":0x1234,
            "supported_buttons":0x89abcdefu32,
        });
        let arrival_event: Event = serde_json::from_value(arrival.clone()).unwrap();
        arrival_event.validate().unwrap();
        assert_eq!(serde_json::to_value(arrival_event).unwrap(), arrival);

        let touch = json!({
            "kind":"controller_touch",
            "number":2,
            "touchpad_index":1,
            "pointer_id":27,
            "phase":"down",
            "x":0.25,
            "y":0.75,
            "pressure":0.5,
        });
        let touch_event: Event = serde_json::from_value(touch.clone()).unwrap();
        touch_event.validate().unwrap();
        assert_eq!(serde_json::to_value(touch_event).unwrap(), touch);

        let motion = json!({
            "kind":"controller_motion",
            "number":2,
            "motion_type":"gyro",
            "x":-1.25,
            "y":0.0,
            "z":9.80665,
        });
        let motion_event: Event = serde_json::from_value(motion.clone()).unwrap();
        motion_event.validate().unwrap();
        let round_tripped: Event =
            serde_json::from_value(serde_json::to_value(&motion_event).unwrap()).unwrap();
        assert_eq!(round_tripped, motion_event);

        let battery = json!({
            "kind":"controller_battery",
            "number":2,
            "state":"charging"
        });
        let battery_event: Event = serde_json::from_value(battery.clone()).unwrap();
        battery_event.validate().unwrap();
        assert_eq!(serde_json::to_value(battery_event).unwrap(), battery);
    }

    #[test]
    fn controller_auxiliary_events_reject_invalid_numbers_coordinates_and_samples() {
        for event in [
            json!({
                "kind":"controller_arrival", "number":16, "controller_type":0,
                "capabilities":0, "supported_buttons":0
            }),
            json!({
                "kind":"controller_touch", "number":0, "touchpad_index":2,
                "pointer_id":1, "phase":"down", "x":0.5, "y":0.5, "pressure":1.0
            }),
            json!({
                "kind":"controller_touch", "number":0, "touchpad_index":0,
                "pointer_id":1, "phase":"down", "x":1.5, "y":0.5, "pressure":1.0
            }),
            json!({
                "kind":"controller_battery", "number":0, "state":"full", "percentage":101
            }),
        ] {
            let event: Event = serde_json::from_value(event).unwrap();
            assert_eq!(event.validate().unwrap_err().code, "invalid_event");
        }
    }

    #[test]
    fn pen_touch_event_round_trips_identity_and_supported_samples() {
        let payload = json!({
            "kind":"pen_touch",
            "pointer_id":PEN_POINTER_ID,
            "device":"pen",
            "phase":"move",
            "x":65535,
            "y":0,
            "pressure":0.75,
            "contact_area_major":0.8_f32,
            "contact_area_minor":0.3_f32,
            "rotation_degrees":359,
            "pen_tool":"unknown",
            "pen_buttons":PEN_BUTTON_PRIMARY | PEN_BUTTON_TERTIARY,
            "tilt":90,
        });
        let event: Event = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(
            event,
            Event::PenTouch {
                pointer_id: PEN_POINTER_ID,
                device: PenTouchDevice::Pen,
                phase: PenTouchPhase::Move,
                x: u16::MAX,
                y: 0,
                pressure: Some(0.75),
                contact_area_major: Some(0.8),
                contact_area_minor: Some(0.3),
                rotation_degrees: Some(359),
                pen_tool: Some(PenTool::Unknown),
                pen_buttons: Some(PEN_BUTTON_PRIMARY | PEN_BUTTON_TERTIARY),
                tilt: Some(90),
            }
        );
        assert!(event.validate().is_ok());
        assert_eq!(serde_json::to_value(event).unwrap(), payload);

        let mut known_zero = serde_json::from_value::<Event>(json!({
            "kind":"pen_touch",
            "pointer_id":PEN_POINTER_ID,
            "device":"pen",
            "phase":"move",
            "x":0,
            "y":0,
            "rotation_degrees":0
        }))
        .unwrap();
        assert!(known_zero.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&known_zero).unwrap()["rotation_degrees"],
            0
        );
        if let Event::PenTouch {
            rotation_degrees, ..
        } = &mut known_zero
        {
            *rotation_degrees = None;
        }
        assert!(
            serde_json::to_value(known_zero)
                .unwrap()
                .get("rotation_degrees")
                .is_none()
        );
    }

    #[test]
    fn pen_touch_preserves_unknown_optional_samples_and_touch_pointer_identity() {
        let payload = json!({
            "kind":"pen_touch",
            "pointer_id":4294967295u32,
            "device":"touch",
            "phase":"down",
            "x":0,
            "y":65535
        });
        let event: Event = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(
            event,
            Event::PenTouch {
                pointer_id: u32::MAX,
                device: PenTouchDevice::Touch,
                phase: PenTouchPhase::Down,
                x: 0,
                y: u16::MAX,
                pressure: None,
                contact_area_major: None,
                contact_area_minor: None,
                rotation_degrees: None,
                pen_tool: None,
                pen_buttons: None,
                tilt: None,
            }
        );
        assert!(event.validate().is_ok());
        assert_eq!(serde_json::to_value(event).unwrap(), payload);
    }

    #[test]
    fn pen_touch_rejects_out_of_range_coordinates_and_non_pen_identity() {
        for coordinate in [-1, 65_536] {
            assert!(
                serde_json::from_value::<Event>(json!({
                    "kind":"pen_touch",
                    "pointer_id":1,
                    "device":"touch",
                    "phase":"down",
                    "x":coordinate,
                    "y":0
                }))
                .is_err()
            );
        }

        assert!(
            serde_json::from_value::<Event>(json!({
                "kind":"pen_touch",
                "pointer_id":4294967296u64,
                "device":"pen",
                "phase":"down",
                "x":0,
                "y":0
            }))
            .is_err()
        );

        let wrong_pen_id: Event = serde_json::from_value(json!({
            "kind":"pen_touch",
            "pointer_id":2,
            "device":"pen",
            "phase":"down",
            "x":0,
            "y":0
        }))
        .unwrap();
        assert_eq!(wrong_pen_id.validate().unwrap_err().code, "invalid_event");
    }

    #[test]
    fn pen_touch_rejects_invalid_rotation_tilt_buttons_and_touch_pen_fields() {
        for (field, value) in [
            ("rotation_degrees", json!(360)),
            ("tilt", json!(91)),
            ("pen_buttons", json!(PEN_BUTTON_MASK | 0b1000)),
        ] {
            let mut payload = json!({
                "kind":"pen_touch",
                "pointer_id":PEN_POINTER_ID,
                "device":"pen",
                "phase":"down",
                "x":0,
                "y":0
            });
            payload[field] = value;
            let event: Event = serde_json::from_value(payload).unwrap();
            assert_eq!(
                event.validate().unwrap_err().code,
                "invalid_event",
                "{field}"
            );
        }

        for field_value in [
            ("pen_tool", json!("eraser")),
            ("pen_buttons", json!(PEN_BUTTON_PRIMARY)),
            ("tilt", json!(0)),
        ] {
            let mut payload = json!({
                "kind":"pen_touch",
                "pointer_id":1,
                "device":"touch",
                "phase":"down",
                "x":0,
                "y":0
            });
            payload[field_value.0] = field_value.1;
            let event: Event = serde_json::from_value(payload).unwrap();
            assert_eq!(event.validate().unwrap_err().code, "invalid_event");
        }

        assert!(
            serde_json::from_value::<Event>(json!({
                "kind":"pen_touch",
                "pointer_id":PEN_POINTER_ID,
                "device":"pen",
                "phase":"down",
                "x":0,
                "y":0,
                "rotation_degrees":-1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<Event>(json!({
                "kind":"pen_touch",
                "pointer_id":PEN_POINTER_ID,
                "device":"pen",
                "phase":"down",
                "x":0,
                "y":0,
                "pen_tool":"brush"
            }))
            .is_err()
        );
    }

    #[test]
    fn pen_touch_rejects_malformed_contact_area_and_pressure() {
        for (major, minor) in [(Some(0.2), None), (None, Some(0.2))] {
            let event = Event::PenTouch {
                pointer_id: 1,
                device: PenTouchDevice::Touch,
                phase: PenTouchPhase::Down,
                x: 0,
                y: 0,
                pressure: None,
                contact_area_major: major,
                contact_area_minor: minor,
                rotation_degrees: None,
                pen_tool: None,
                pen_buttons: None,
                tilt: None,
            };
            assert_eq!(event.validate().unwrap_err().code, "invalid_event");
        }

        for sample in [
            ("pressure", f32::NAN),
            ("pressure", f32::INFINITY),
            ("pressure", f32::NEG_INFINITY),
            ("pressure", -0.01),
            ("pressure", 1.01),
            ("contact area", f32::NAN),
            ("contact area", f32::INFINITY),
            ("contact area", -0.01),
            ("contact area", 1.01),
        ] {
            let event = Event::PenTouch {
                pointer_id: 1,
                device: PenTouchDevice::Touch,
                phase: PenTouchPhase::Down,
                x: 0,
                y: 0,
                pressure: (sample.0 == "pressure").then_some(sample.1),
                contact_area_major: (sample.0 == "contact area").then_some(sample.1),
                contact_area_minor: (sample.0 == "contact area").then_some(sample.1),
                rotation_degrees: None,
                pen_tool: None,
                pen_buttons: None,
                tilt: None,
            };
            assert_eq!(
                event.validate().unwrap_err().code,
                "invalid_event",
                "{}",
                sample.0
            );
        }
    }

    #[test]
    fn pen_tilt_axes_use_polar_rotation_and_require_known_valid_samples() {
        let assert_axes = |rotation: u16, expected_x: f32, expected_y: f32| {
            let (x, y) = pen_tilt_axes(Some(45), Some(rotation)).unwrap();
            assert!((x - expected_x).abs() < 0.001, "rotation {rotation}: x={x}");
            assert!((y - expected_y).abs() < 0.001, "rotation {rotation}: y={y}");
        };
        assert_axes(0, 0.0, 45.0);
        assert_axes(90, -45.0, 0.0);
        assert_axes(180, 0.0, -45.0);
        assert_axes(270, 45.0, 0.0);
        assert_eq!(pen_tilt_axes(None, Some(90)), None);
        assert_eq!(pen_tilt_axes(Some(45), None), None);
        assert_eq!(pen_tilt_axes(Some(91), Some(0)), None);
        assert_eq!(pen_tilt_axes(Some(45), Some(360)), None);
        assert_eq!(pen_tilt_axes(Some(0), Some(359)), Some((0.0, 0.0)));
    }
}
