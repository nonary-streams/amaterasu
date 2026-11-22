//! Semantic input and host feedback shared between session and control providers.
use std::borrow::Cow;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Input<'a> {
    Keyboard {
        pressed: bool,
        key_code: u16,
        modifiers: u8,
        flags: u8,
    },
    RelativeMouse {
        x: i16,
        y: i16,
    },
    AbsoluteMouse {
        x: u16,
        y: u16,
        reference_width: u32,
        reference_height: u32,
    },
    MouseButton {
        pressed: bool,
        button: u8,
    },
    VerticalScroll {
        amount: i16,
    },
    HorizontalScroll {
        amount: i16,
    },
    Text(Cow<'a, str>),
    Controller(ControllerState),
    ControllerArrival(ControllerArrival),
    ControllerTouch(ControllerTouch),
    ControllerMotion(ControllerMotion),
    ControllerBattery(ControllerBattery),
    PenTouch(PenTouch),
    EnableHaptics {
        enabled: bool,
    },
    Unsupported {
        magic: u32,
        payload: Cow<'a, [u8]>,
    },
}

/// Authenticated pen or touch input. Floating fields carry IEEE-754 bits
/// so malformed values can be rejected at ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PenTouch {
    pub device: PointerDevice,
    pub event: PointerEvent,
    pub pointer_id: u32,
    pub x: u32,
    pub y: u32,
    pub pressure: u32,
    pub contact_major: u32,
    pub contact_minor: u32,
    pub rotation: u16,
    pub tool: Option<u8>,
    pub buttons: Option<u8>,
    pub tilt: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PointerDevice {
    Touch,
    Pen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PointerEvent {
    Down,
    Up,
    Move,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControllerState {
    pub number: u16,
    pub active_mask: u16,
    pub buttons: u32,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControllerArrival {
    pub number: u8,
    pub controller_type: u8,
    pub capabilities: u16,
    pub supported_buttons: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControllerTouch {
    pub number: u8,
    pub touchpad_index: u8,
    pub event: ControllerTouchEvent,
    pub pointer_id: u32,
    pub x: u32,
    pub y: u32,
    pub pressure: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ControllerTouchEvent {
    Hover,
    Down,
    Up,
    Move,
    Cancel,
    ButtonOnly,
    HoverLeave,
    CancelAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControllerMotion {
    pub number: u8,
    pub motion_type: ControllerMotionType,
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControllerMotionType {
    Accel,
    Gyro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControllerBattery {
    pub number: u8,
    pub state: ControllerBatteryState,
    pub percentage: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ControllerBatteryState {
    Unknown,
    NotPresent,
    Discharging,
    Charging,
    NotCharging,
    Full,
}

pub const MAX_CONTROLLER_SLOT: u16 = 16;
pub const MAX_HAPTICS_FRAMES: u16 = 240;
pub const HAPTICS_FRAME_BYTES: usize = 4;
pub const ADAPTIVE_TRIGGER_EFFECT_BYTES: usize = 10;

/// Output from a native controller backend, addressed by client-relative slot.
/// Slot ownership must be established by the input lease before it reaches here.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostFeedback {
    Rumble {
        controller: u16,
        low_frequency: u16,
        high_frequency: u16,
    },
    TriggerRumble {
        controller: u16,
        left: u16,
        right: u16,
    },
    Motion {
        controller: u16,
        report_rate_hz: u16,
        motion_type: ControllerMotionType,
    },
    RgbLed {
        controller: u16,
        red: u8,
        green: u8,
        blue: u8,
    },
    AdaptiveTriggers {
        controller: u16,
        event_flags: u8,
        type_left: u8,
        type_right: u8,
        left: [u8; ADAPTIVE_TRIGGER_EFFECT_BYTES],
        right: [u8; ADAPTIVE_TRIGGER_EFFECT_BYTES],
    },
    HapticsPcm {
        controller: u16,
        sequence: u32,
        frames: u16,
        /// Interleaved left/right actuator samples, signed 16-bit little endian.
        samples_s16le: Vec<u8>,
    },
}

/// Delivery requested by the provider for a framed host event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFeedbackDelivery {
    Reliable,
    SequencedUnreliable,
    Unsequenced,
}

/// Owned framed record. The selected transport owns encryption and sequencing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFeedbackMessage {
    pub message_type: u16,
    pub channel: u8,
    pub delivery: HostFeedbackDelivery,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackEncodeError {
    InvalidController,
    InvalidAdaptiveTriggerFlags,
    InvalidHapticsFrameCount,
    InvalidHapticsSampleLength,
}

/// Semantic input produced after the selected control provider authenticates and parses a record.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "input", rename_all = "snake_case")]
pub enum ClientControl<'a> {
    Keepalive,
    RequestIdr,
    Input(Input<'a>),
    Unsupported,
    Invalid,
}

/// Host-originated events that the selected control provider frames.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum HostControl {
    Feedback(HostFeedback),
    HdrMetadata { fields: [u16; 12] },
}
