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
    EnableHaptics {
        enabled: bool,
    },
    Unsupported {
        magic: u32,
        payload: &'a [u8],
    },
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
        _ => Ok(Input::Unsupported {
            magic,
            payload: body,
        }),
    }
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
