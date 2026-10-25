//! Typed inner messages. Call only after modern control GCM authentication.
mod feedback;
mod input;
mod rumble;
#[cfg(test)]
mod tests;
mod translate;

use crate::crypto::{Direction, MAX_CONTROL_PAYLOAD_BYTES};
pub use feedback::{
    FeedbackEncodeError, HostFeedback, HostFeedbackDelivery, HostFeedbackMessage,
    encode_host_feedback,
};
pub use input::{
    ControllerArrival, ControllerBattery, ControllerBatteryState, ControllerMotion,
    ControllerMotionType, ControllerState, ControllerTouch, ControllerTouchEvent, Input, PenTouch,
    PointerDevice, PointerEvent,
};
pub use rumble::{
    MAX_CONTROLLER_SLOT, RUMBLE_MESSAGE_TYPE, RumbleFeedback, RumbleFeedbackError,
    encode_rumble_feedback,
};
pub use translate::{
    AbsoluteViewport, InputBatch, InputEvent, InputTranslator, KeyState as TranslatedKeyState,
    MouseButton, ScrollAxis, TranslationError, VirtualKeyMapping, virtual_key_mapping,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    /// Sunshine modern encrypted control with application version >= 7.1.431.
    SunshineModern,
    Legacy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    UnsupportedProfile,
    WrongDirection,
    InvalidLength,
    InvalidValue,
    InvalidText,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Message<'a> {
    Ping {
        timestamp: u32,
        opaque_tail: [u8; 2],
    },
    Start,
    RequestIdr,
    InvalidateReferences {
        first: u32,
        last: u32,
    },
    LongTermReferenceAcknowledged {
        frame: u32,
    },
    Termination {
        reason: u32,
    },
    FecFeedback(FecFeedback),
    Input(Input<'a>),
    Unsupported {
        kind: u16,
        payload: &'a [u8],
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct FecFeedback {
    pub frame: u32,
    pub highest_received_sequence: u16,
    pub next_contiguous_sequence: u16,
    pub missing_before_highest: u16,
    pub total_data: u16,
    pub total_parity: u16,
    pub received_data: u16,
    pub received_parity: u16,
    pub percentage: u8,
    pub block_index: u8,
    pub block_count: u8,
}

/// `direction` comes from the authenticated session, never an ENet channel.
/// The profile asserts negotiated application version and modern encryption;
/// this pure parser cannot establish either assertion or authorize input effects.
pub fn decode(
    profile: Profile,
    direction: Direction,
    kind: u16,
    payload: &[u8],
) -> Result<Message<'_>, DecodeError> {
    if profile != Profile::SunshineModern {
        return Err(DecodeError::UnsupportedProfile);
    }
    if payload.len() > MAX_CONTROL_PAYLOAD_BYTES {
        return Err(DecodeError::InvalidLength);
    }
    if direction == Direction::HostToClient {
        return if kind == 0x0109 {
            exact(payload, 4)?;
            Ok(Message::Termination {
                reason: be32(payload, 0),
            })
        } else {
            Ok(Message::Unsupported { kind, payload })
        };
    }
    match kind {
        0x0109 => Err(DecodeError::WrongDirection),
        0x0200 => ping(payload),
        0x0302 => {
            constant(payload, &[0, 0])?;
            Ok(Message::RequestIdr)
        }
        0x0307 => {
            constant(payload, &[0])?;
            Ok(Message::Start)
        }
        0x0301 => references(payload),
        0x0350 => {
            exact(payload, 8)?;
            constant(&payload[4..], &[0; 4])?;
            Ok(Message::LongTermReferenceAcknowledged {
                frame: le32(payload, 0),
            })
        }
        0x5502 => feedback(payload).map(Message::FecFeedback),
        0x0206 => input::decode(payload).map(Message::Input),
        _ => Ok(Message::Unsupported { kind, payload }),
    }
}

fn ping(bytes: &[u8]) -> Result<Message<'_>, DecodeError> {
    exact(bytes, 8)?;
    constant(&bytes[..2], &[4, 0])?;
    // The pinned primary client does not initialize these last two bytes.
    Ok(Message::Ping {
        timestamp: le32(bytes, 2),
        opaque_tail: [bytes[6], bytes[7]],
    })
}

fn references(bytes: &[u8]) -> Result<Message<'_>, DecodeError> {
    exact(bytes, 24)?;
    constant(&bytes[4..8], &[0; 4])?;
    constant(&bytes[12..], &[0; 12])?;
    let first = le32(bytes, 0);
    let last = le32(bytes, 8);
    if first > last {
        return Err(DecodeError::InvalidValue);
    }
    Ok(Message::InvalidateReferences { first, last })
}

fn feedback(bytes: &[u8]) -> Result<FecFeedback, DecodeError> {
    exact(bytes, 21)?;
    let value = FecFeedback {
        frame: be32(bytes, 0),
        highest_received_sequence: be16(bytes, 4),
        next_contiguous_sequence: be16(bytes, 6),
        missing_before_highest: be16(bytes, 8),
        total_data: be16(bytes, 10),
        total_parity: be16(bytes, 12),
        received_data: be16(bytes, 14),
        received_parity: be16(bytes, 16),
        percentage: bytes[18],
        block_index: bytes[19],
        block_count: bytes[20],
    };
    if !(1..=4).contains(&value.block_count)
        || value.block_index >= value.block_count
        || value.received_data > value.total_data
        || value.received_parity > value.total_parity
    {
        return Err(DecodeError::InvalidValue);
    }
    Ok(value)
}

fn exact(bytes: &[u8], length: usize) -> Result<(), DecodeError> {
    if bytes.len() == length {
        Ok(())
    } else {
        Err(DecodeError::InvalidLength)
    }
}

fn constant(bytes: &[u8], expected: &[u8]) -> Result<(), DecodeError> {
    exact(bytes, expected.len())?;
    if bytes == expected {
        Ok(())
    } else {
        Err(DecodeError::InvalidValue)
    }
}

// These helpers are called only after their enclosing fixed-size record is checked.
fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn be16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn be32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
