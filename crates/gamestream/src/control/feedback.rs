//! Validated GameStream host-to-client controller feedback records.
use super::ControllerMotionType;

pub const MAX_CONTROLLER_SLOT: u16 = 16;
pub const MAX_HAPTICS_FRAMES: u16 = 240;
pub const HAPTICS_FRAME_BYTES: usize = 4;
pub const ADAPTIVE_TRIGGER_EFFECT_BYTES: usize = 10;

const RUMBLE: u16 = 0x010b;
const TRIGGER_RUMBLE: u16 = 0x5500;
const MOTION_STATE: u16 = 0x5501;
const RGB_LED: u16 = 0x5502;
const ADAPTIVE_TRIGGERS: u16 = 0x5503;
const HAPTICS_PCM: u16 = 0x5601;
const HAPTICS_CHANNEL: u8 = 0x08;
const ADAPTIVE_TRIGGER_EVENT_FLAGS: u8 = 0x0c;

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

/// ENet delivery for the encoded control event. Unreliable means sequenced,
/// matching the haptics extension's real-time PCM route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFeedbackDelivery {
    Reliable,
    SequencedUnreliable,
}

/// Owned inner control record; encryption and nonce assignment remain with the
/// authenticated control-transport provider.
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

/// Encodes one client-relative feedback record without constructing its outer
/// authenticated envelope. The control transport adds that envelope and owns
/// the sequence number after session authentication.
pub fn encode_host_feedback(
    feedback: HostFeedback,
) -> Result<HostFeedbackMessage, FeedbackEncodeError> {
    let (message_type, channel, delivery, payload) = match feedback {
        HostFeedback::Rumble {
            controller,
            low_frequency,
            high_frequency,
        } => {
            let mut payload = Vec::with_capacity(10);
            payload.extend_from_slice(&0x00c0_ffee_u32.to_le_bytes());
            push_controller(&mut payload, controller)?;
            payload.extend_from_slice(&low_frequency.to_le_bytes());
            payload.extend_from_slice(&high_frequency.to_le_bytes());
            (RUMBLE, 0, HostFeedbackDelivery::Reliable, payload)
        }
        HostFeedback::TriggerRumble {
            controller,
            left,
            right,
        } => {
            let mut payload = Vec::with_capacity(6);
            push_controller(&mut payload, controller)?;
            payload.extend_from_slice(&left.to_le_bytes());
            payload.extend_from_slice(&right.to_le_bytes());
            (TRIGGER_RUMBLE, 0, HostFeedbackDelivery::Reliable, payload)
        }
        HostFeedback::Motion {
            controller,
            report_rate_hz,
            motion_type,
        } => {
            let mut payload = Vec::with_capacity(5);
            push_controller(&mut payload, controller)?;
            payload.extend_from_slice(&report_rate_hz.to_le_bytes());
            payload.push(match motion_type {
                ControllerMotionType::Accel => 0x01,
                ControllerMotionType::Gyro => 0x02,
            });
            (MOTION_STATE, 0, HostFeedbackDelivery::Reliable, payload)
        }
        HostFeedback::RgbLed {
            controller,
            red,
            green,
            blue,
        } => {
            let mut payload = Vec::with_capacity(5);
            push_controller(&mut payload, controller)?;
            payload.extend_from_slice(&[red, green, blue]);
            (RGB_LED, 0, HostFeedbackDelivery::Reliable, payload)
        }
        HostFeedback::AdaptiveTriggers {
            controller,
            event_flags,
            type_left,
            type_right,
            left,
            right,
        } => {
            let mut payload = Vec::with_capacity(25);
            push_controller(&mut payload, controller)?;
            if event_flags & !ADAPTIVE_TRIGGER_EVENT_FLAGS != 0 {
                return Err(FeedbackEncodeError::InvalidAdaptiveTriggerFlags);
            }
            payload.extend_from_slice(&[event_flags, type_left, type_right]);
            payload.extend_from_slice(&left);
            payload.extend_from_slice(&right);
            (
                ADAPTIVE_TRIGGERS,
                0,
                HostFeedbackDelivery::Reliable,
                payload,
            )
        }
        HostFeedback::HapticsPcm {
            controller,
            sequence,
            frames,
            samples_s16le,
        } => {
            if !(1..=MAX_HAPTICS_FRAMES).contains(&frames) {
                return Err(FeedbackEncodeError::InvalidHapticsFrameCount);
            }
            let expected_bytes = usize::from(frames) * HAPTICS_FRAME_BYTES;
            if samples_s16le.len() != expected_bytes {
                return Err(FeedbackEncodeError::InvalidHapticsSampleLength);
            }
            let mut payload = Vec::with_capacity(12 + expected_bytes);
            payload.extend_from_slice(&[1, 1]);
            push_controller(&mut payload, controller)?;
            payload.extend_from_slice(&sequence.to_le_bytes());
            payload.extend_from_slice(&frames.to_le_bytes());
            payload.extend_from_slice(&[0, 0]);
            payload.extend_from_slice(&samples_s16le);
            (
                HAPTICS_PCM,
                HAPTICS_CHANNEL,
                HostFeedbackDelivery::SequencedUnreliable,
                payload,
            )
        }
    };
    Ok(HostFeedbackMessage {
        message_type,
        channel,
        delivery,
        payload,
    })
}

fn push_controller(payload: &mut Vec<u8>, controller: u16) -> Result<(), FeedbackEncodeError> {
    if controller >= MAX_CONTROLLER_SLOT {
        return Err(FeedbackEncodeError::InvalidController);
    }
    payload.extend_from_slice(&controller.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_feedback_records_match_sunshine_payload_bytes() {
        let cases = [
            (
                HostFeedback::Rumble {
                    controller: 3,
                    low_frequency: 0x1234,
                    high_frequency: 0xabcd,
                },
                0x010b,
                vec![0xee, 0xff, 0xc0, 0, 3, 0, 0x34, 0x12, 0xcd, 0xab],
            ),
            (
                HostFeedback::TriggerRumble {
                    controller: 3,
                    left: 0x1234,
                    right: 0xabcd,
                },
                0x5500,
                vec![3, 0, 0x34, 0x12, 0xcd, 0xab],
            ),
            (
                HostFeedback::Motion {
                    controller: 2,
                    report_rate_hz: 100,
                    motion_type: ControllerMotionType::Gyro,
                },
                0x5501,
                vec![2, 0, 100, 0, 2],
            ),
            (
                HostFeedback::RgbLed {
                    controller: 4,
                    red: 0x12,
                    green: 0x34,
                    blue: 0x56,
                },
                0x5502,
                vec![4, 0, 0x12, 0x34, 0x56],
            ),
        ];
        for (feedback, expected_type, expected_payload) in cases {
            let message = encode_host_feedback(feedback).unwrap();
            assert_eq!(message.message_type, expected_type);
            assert_eq!(message.channel, 0);
            assert_eq!(message.delivery, HostFeedbackDelivery::Reliable);
            assert_eq!(message.payload, expected_payload);
        }
    }

    #[test]
    fn adaptive_trigger_record_preserves_types_and_opaque_effect_bytes() {
        let left = [0x11; ADAPTIVE_TRIGGER_EFFECT_BYTES];
        let right = [0x22; ADAPTIVE_TRIGGER_EFFECT_BYTES];
        let message = encode_host_feedback(HostFeedback::AdaptiveTriggers {
            controller: 1,
            event_flags: 0x0c,
            type_left: 7,
            type_right: 9,
            left,
            right,
        })
        .unwrap();
        assert_eq!(message.message_type, 0x5503);
        assert_eq!(message.channel, 0);
        assert_eq!(message.delivery, HostFeedbackDelivery::Reliable);
        assert_eq!(message.payload.len(), 25);
        assert_eq!(&message.payload[..5], &[1, 0, 0x0c, 7, 9]);
        assert_eq!(&message.payload[5..15], &left);
        assert_eq!(&message.payload[15..], &right);
    }

    #[test]
    fn pcm_record_has_versioned_little_endian_stereo_header_and_unreliable_route() {
        let message = encode_host_feedback(HostFeedback::HapticsPcm {
            controller: 15,
            sequence: 0x1234_5678,
            frames: 2,
            samples_s16le: vec![1, 2, 3, 4, 5, 6, 7, 8],
        })
        .unwrap();
        assert_eq!(message.message_type, 0x5601);
        assert_eq!(message.channel, 8);
        assert_eq!(message.delivery, HostFeedbackDelivery::SequencedUnreliable);
        assert_eq!(
            message.payload,
            [
                1, 1, 15, 0, 0x78, 0x56, 0x34, 0x12, 2, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8
            ]
        );
    }

    #[test]
    fn malformed_feedback_values_are_rejected_before_serialization() {
        assert_eq!(
            encode_host_feedback(HostFeedback::Rumble {
                controller: 16,
                low_frequency: 0,
                high_frequency: 0,
            }),
            Err(FeedbackEncodeError::InvalidController)
        );
        assert_eq!(
            encode_host_feedback(HostFeedback::AdaptiveTriggers {
                controller: 0,
                event_flags: 1,
                type_left: 0,
                type_right: 0,
                left: [0; ADAPTIVE_TRIGGER_EFFECT_BYTES],
                right: [0; ADAPTIVE_TRIGGER_EFFECT_BYTES],
            }),
            Err(FeedbackEncodeError::InvalidAdaptiveTriggerFlags)
        );
        for (frames, samples, expected) in [
            (0, vec![], FeedbackEncodeError::InvalidHapticsFrameCount),
            (
                241,
                vec![0; 241 * HAPTICS_FRAME_BYTES],
                FeedbackEncodeError::InvalidHapticsFrameCount,
            ),
            (
                1,
                vec![0; 3],
                FeedbackEncodeError::InvalidHapticsSampleLength,
            ),
        ] {
            assert_eq!(
                encode_host_feedback(HostFeedback::HapticsPcm {
                    controller: 0,
                    sequence: 0,
                    frames,
                    samples_s16le: samples,
                }),
                Err(expected)
            );
        }
    }

    #[test]
    fn host_feedback_uses_strict_tagged_json_for_the_input_pull_contract() {
        let event: HostFeedback = serde_json::from_str(
            r#"{"type":"motion","controller":2,"report_rate_hz":100,"motion_type":"gyro"}"#,
        )
        .unwrap();
        assert_eq!(
            event,
            HostFeedback::Motion {
                controller: 2,
                report_rate_hz: 100,
                motion_type: ControllerMotionType::Gyro,
            }
        );
        assert!(serde_json::from_str::<HostFeedback>(
            r#"{"type":"rumble","controller":0,"low_frequency":1,"high_frequency":2,"ignored":true}"#
        )
        .is_err());
    }
}
