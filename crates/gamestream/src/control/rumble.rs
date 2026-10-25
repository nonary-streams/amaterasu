//! Moonlight host-to-client standard controller rumble record.

pub const MAX_CONTROLLER_SLOT: u16 = 16;
pub const RUMBLE_MESSAGE_TYPE: u16 = 0x010b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RumbleFeedback {
    pub slot: u16,
    pub low_frequency: u16,
    pub high_frequency: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RumbleFeedbackError {
    InvalidController,
}

/// Encodes the authenticated inner payload; control-transport supplies the
/// reliable channel-0 envelope and assigns its encryption sequence.
pub fn encode_rumble_feedback(feedback: RumbleFeedback) -> Result<[u8; 10], RumbleFeedbackError> {
    if feedback.slot >= MAX_CONTROLLER_SLOT {
        return Err(RumbleFeedbackError::InvalidController);
    }
    let mut payload = [0u8; 10];
    // Moonlight's ControlStream.c skips four bytes before reading the LE16
    // controller/low/high values. The compatible host reference writes C0FFEE.
    payload[..4].copy_from_slice(&0x00c0_ffee_u32.to_le_bytes());
    payload[4..6].copy_from_slice(&feedback.slot.to_le_bytes());
    payload[6..8].copy_from_slice(&feedback.low_frequency.to_le_bytes());
    payload[8..10].copy_from_slice(&feedback.high_frequency.to_le_bytes());
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble_payload_uses_client_wire_prefix_and_low_high_order() {
        let payload = encode_rumble_feedback(RumbleFeedback {
            slot: 3,
            low_frequency: 0x1234,
            high_frequency: 0xabcd,
        })
        .unwrap();
        assert_eq!(payload, [0xee, 0xff, 0xc0, 0, 3, 0, 0x34, 0x12, 0xcd, 0xab]);
    }

    #[test]
    fn rumble_feedback_rejects_slots_outside_moonlight_range() {
        for slot in [MAX_CONTROLLER_SLOT, u16::MAX] {
            assert_eq!(
                encode_rumble_feedback(RumbleFeedback {
                    slot,
                    low_frequency: 0,
                    high_frequency: 0,
                }),
                Err(RumbleFeedbackError::InvalidController)
            );
        }
    }
}
