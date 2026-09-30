use super::{DecodeError, be16, be32, constant, exact, le16, le32};

#[derive(Debug, PartialEq, Eq)]
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
    Text(&'a str),
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
        payload: &'a [u8],
    },
}

/// Authenticated Moonlight pen/touch payload. Floating fields remain as their
/// little-endian IEEE-754 bits so malformed values can be rejected at ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerDevice {
    Touch,
    Pen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerEvent {
    Down,
    Up,
    Move,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerArrival {
    pub number: u8,
    pub controller_type: u8,
    pub capabilities: u16,
    pub supported_buttons: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerTouch {
    pub number: u8,
    pub touchpad_index: u8,
    pub event: ControllerTouchEvent,
    pub pointer_id: u32,
    pub x: u32,
    pub y: u32,
    pub pressure: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerBattery {
    pub number: u8,
    pub state: ControllerBatteryState,
    pub percentage: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerBatteryState {
    Unknown,
    NotPresent,
    Discharging,
    Charging,
    NotCharging,
    Full,
}

pub(super) fn decode(bytes: &[u8]) -> Result<Input<'_>, DecodeError> {
    if bytes.len() < 8 || be32(bytes, 0) as usize != bytes.len() - 4 {
        return Err(DecodeError::InvalidLength);
    }
    let magic = le32(bytes, 4);
    let body = &bytes[8..];
    match magic {
        3 | 4 => keyboard(magic == 3, body),
        7 => {
            exact(body, 4)?;
            Ok(Input::RelativeMouse {
                x: be16(body, 0) as i16,
                y: be16(body, 2) as i16,
            })
        }
        5 => absolute(body),
        8 | 9 => {
            exact(body, 1)?;
            if !(1..=5).contains(&body[0]) {
                return Err(DecodeError::InvalidValue);
            }
            Ok(Input::MouseButton {
                pressed: magic == 8,
                button: body[0],
            })
        }
        10 => {
            exact(body, 6)?;
            constant(&body[2..4], &body[..2])?;
            constant(&body[4..], &[0, 0])?;
            Ok(Input::VerticalScroll {
                amount: be16(body, 0) as i16,
            })
        }
        0x55000001 => {
            exact(body, 2)?;
            Ok(Input::HorizontalScroll {
                amount: be16(body, 0) as i16,
            })
        }
        0x17 => text(body),
        0x0c => controller(body).map(Input::Controller),
        0x55000004 => controller_arrival(body).map(Input::ControllerArrival),
        0x55000005 => controller_touch(body).map(Input::ControllerTouch),
        0x55000006 => controller_motion(body).map(Input::ControllerMotion),
        0x55000007 => controller_battery(body).map(Input::ControllerBattery),
        0x0d => {
            exact(body, 2)?;
            let enabled = le16(body, 0);
            if enabled > 1 {
                return Err(DecodeError::InvalidValue);
            }
            Ok(Input::EnableHaptics {
                enabled: enabled == 1,
            })
        }
        0x55000002 | 0x55000003 => pen_touch(magic, body).map(Input::PenTouch),
        _ => Ok(Input::Unsupported {
            magic,
            payload: body,
        }),
    }
}

fn controller_arrival(body: &[u8]) -> Result<ControllerArrival, DecodeError> {
    exact(body, 8)?;
    let number = body[0];
    if number >= 16 {
        return Err(DecodeError::InvalidValue);
    }
    Ok(ControllerArrival {
        number,
        controller_type: body[1],
        capabilities: le16(body, 2),
        supported_buttons: le32(body, 4),
    })
}

fn controller_touch(body: &[u8]) -> Result<ControllerTouch, DecodeError> {
    exact(body, 20)?;
    let number = body[0];
    let event = match body[1] {
        0 => ControllerTouchEvent::Hover,
        1 => ControllerTouchEvent::Down,
        2 => ControllerTouchEvent::Up,
        3 => ControllerTouchEvent::Move,
        4 => ControllerTouchEvent::Cancel,
        5 => ControllerTouchEvent::ButtonOnly,
        6 => ControllerTouchEvent::HoverLeave,
        7 => ControllerTouchEvent::CancelAll,
        _ => return Err(DecodeError::InvalidValue),
    };
    let touchpad_index = body[3];
    if number >= 16 || body[2] != 0 || touchpad_index > 1 {
        return Err(DecodeError::InvalidValue);
    }
    let (x, y, pressure) = (le32(body, 8), le32(body, 12), le32(body, 16));
    if [x, y, pressure]
        .into_iter()
        .any(|bits| !f32::from_bits(bits).is_finite())
    {
        return Err(DecodeError::InvalidValue);
    }
    Ok(ControllerTouch {
        number,
        touchpad_index,
        event,
        pointer_id: le32(body, 4),
        x,
        y,
        pressure,
    })
}

fn controller_motion(body: &[u8]) -> Result<ControllerMotion, DecodeError> {
    exact(body, 16)?;
    let number = body[0];
    let motion_type = match body[1] {
        1 => ControllerMotionType::Accel,
        2 => ControllerMotionType::Gyro,
        _ => return Err(DecodeError::InvalidValue),
    };
    if number >= 16 || body[2..4] != [0, 0] {
        return Err(DecodeError::InvalidValue);
    }
    let (x, y, z) = (le32(body, 4), le32(body, 8), le32(body, 12));
    if [x, y, z]
        .into_iter()
        .any(|bits| !f32::from_bits(bits).is_finite())
    {
        return Err(DecodeError::InvalidValue);
    }
    Ok(ControllerMotion {
        number,
        motion_type,
        x,
        y,
        z,
    })
}

fn controller_battery(body: &[u8]) -> Result<ControllerBattery, DecodeError> {
    exact(body, 4)?;
    let number = body[0];
    let state = match body[1] {
        0 => ControllerBatteryState::Unknown,
        1 => ControllerBatteryState::NotPresent,
        2 => ControllerBatteryState::Discharging,
        3 => ControllerBatteryState::Charging,
        4 => ControllerBatteryState::NotCharging,
        5 => ControllerBatteryState::Full,
        _ => return Err(DecodeError::InvalidValue),
    };
    let percentage = match body[2] {
        0..=100 => Some(body[2]),
        0xff => None,
        _ => return Err(DecodeError::InvalidValue),
    };
    if number >= 16 || body[3] != 0 {
        return Err(DecodeError::InvalidValue);
    }
    Ok(ControllerBattery {
        number,
        state,
        percentage,
    })
}

fn pen_touch(magic: u32, body: &[u8]) -> Result<PenTouch, DecodeError> {
    exact(body, 28)?;
    let event = match body[0] {
        1 => PointerEvent::Down,
        2 => PointerEvent::Up,
        3 => PointerEvent::Move,
        4 => PointerEvent::Cancel,
        _ => return Err(DecodeError::InvalidValue),
    };
    let pen = magic == 0x55000003;
    if (pen
        && (body[3] != 0
            || body[19] != 0
            || body[1] > 2
            || body[2] & !7 != 0
            || (body[18] > 90 && body[18] != 0xff)))
        || (!pen && body[1] != 0)
    {
        return Err(DecodeError::InvalidValue);
    }
    let (x, y, pressure, major, minor, rotation) = if pen {
        (
            le32(body, 4),
            le32(body, 8),
            le32(body, 12),
            le32(body, 20),
            le32(body, 24),
            le16(body, 16),
        )
    } else {
        (
            le32(body, 8),
            le32(body, 12),
            le32(body, 16),
            le32(body, 20),
            le32(body, 24),
            le16(body, 2),
        )
    };
    if [x, y, pressure, major, minor]
        .into_iter()
        .any(|bits| !f32::from_bits(bits).is_finite())
    {
        return Err(DecodeError::InvalidValue);
    }
    Ok(PenTouch {
        device: if pen {
            PointerDevice::Pen
        } else {
            PointerDevice::Touch
        },
        event,
        pointer_id: if pen { 1 } else { le32(body, 4) },
        x,
        y,
        pressure,
        contact_major: major,
        contact_minor: minor,
        rotation,
        tool: pen.then_some(body[1]),
        buttons: pen.then_some(body[2]),
        tilt: pen.then_some(body[18]),
    })
}

fn keyboard(pressed: bool, body: &[u8]) -> Result<Input<'_>, DecodeError> {
    exact(body, 6)?;
    constant(&body[4..], &[0, 0])?;
    // Unknown flag bits are rejected rather than interpreted as an input action.
    if body[0] & !1 != 0 || body[3] & !15 != 0 {
        return Err(DecodeError::InvalidValue);
    }
    Ok(Input::Keyboard {
        pressed,
        flags: body[0],
        key_code: le16(body, 1),
        modifiers: body[3],
    })
}

fn absolute(body: &[u8]) -> Result<Input<'_>, DecodeError> {
    exact(body, 10)?;
    constant(&body[4..6], &[0, 0])?;
    let (x, y, width, height) = (be16(body, 0), be16(body, 2), be16(body, 6), be16(body, 8));
    if x > width || y > height {
        return Err(DecodeError::InvalidValue);
    }
    // The wire stores inclusive maximum coordinates, not pixel counts.
    Ok(Input::AbsoluteMouse {
        x,
        y,
        reference_width: u32::from(width) + 1,
        reference_height: u32::from(height) + 1,
    })
}

fn text(body: &[u8]) -> Result<Input<'_>, DecodeError> {
    if body.is_empty() || body.len() > 32 {
        return Err(DecodeError::InvalidLength);
    }
    let text = std::str::from_utf8(body).map_err(|_| DecodeError::InvalidText)?;
    if text.contains('\0') {
        return Err(DecodeError::InvalidText);
    }
    Ok(Input::Text(text))
}

fn controller(body: &[u8]) -> Result<ControllerState, DecodeError> {
    exact(body, 26)?;
    if le16(body, 0) != 0x1a
        || le16(body, 6) != 0x14
        || le16(body, 20) != 0x9c
        || le16(body, 24) != 0x55
        || le16(body, 2) >= 16
    {
        return Err(DecodeError::InvalidValue);
    }
    Ok(ControllerState {
        number: le16(body, 2),
        active_mask: le16(body, 4),
        buttons: u32::from(le16(body, 8)) | (u32::from(le16(body, 22)) << 16),
        left_trigger: body[10],
        right_trigger: body[11],
        left_x: le16(body, 12) as i16,
        left_y: le16(body, 14) as i16,
        right_x: le16(body, 16) as i16,
        right_y: le16(body, 18) as i16,
    })
}
