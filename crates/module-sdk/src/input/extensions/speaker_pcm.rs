//! Optional 48 kHz stereo controller-speaker blocks, not actuator haptics.
//! Source: Vibeshine `eeab9d96c4db45de511022d451810dd2c76510f3`,
//! `src/controller_speaker_pcm.h:14-25,95-136` and
//! `src/platform/windows/dualsense_usbip_gamepad_policy.h:118-150`.
//! This internal payload is not the encrypted GameStream packet.

use super::{Capability, Direction};

pub const SCHEMA: &str = "nonary.controller-speaker.pcm";
pub const SAMPLE_RATE_HZ: u32 = 48_000;
pub const FRAMES: u16 = 240;
pub const SAMPLE_BYTES: usize = 960;
pub const PAYLOAD_BYTES: usize = 986;
pub const START: u16 = 1;

pub fn capability() -> Capability {
    Capability {
        schema: SCHEMA.into(),
        version: 1,
        direction: Direction::ToClient,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Block {
    /// Bound by the owning protocol adapter; it is not a standalone grant.
    pub owner: u64,
    pub generation: u32,
    pub sequence: u32,
    /// Offset in stereo sample frames, not bytes or milliseconds.
    pub sample_position: u64,
    pub flags: u16,
    /// Exactly 240 interleaved stereo signed 16-bit little-endian frames.
    pub samples: [u8; SAMPLE_BYTES],
}

impl Block {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.owner == 0
            || self.generation == 0
            || self.flags & !START != 0
            || self
                .sample_position
                .checked_add(u64::from(FRAMES))
                .is_none()
        {
            return Err("invalid_speaker_block");
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        self.validate()?;
        let mut bytes = Vec::with_capacity(PAYLOAD_BYTES);
        bytes.extend_from_slice(&self.owner.to_le_bytes());
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.sample_position.to_le_bytes());
        bytes.extend_from_slice(&self.flags.to_le_bytes());
        bytes.extend_from_slice(&self.samples);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != PAYLOAD_BYTES {
            return Err("invalid_speaker_length");
        }
        let block = Self {
            owner: u64::from_le_bytes(bytes[0..8].try_into().expect("fixed length checked")),
            generation: u32::from_le_bytes(bytes[8..12].try_into().expect("fixed length checked")),
            sequence: u32::from_le_bytes(bytes[12..16].try_into().expect("fixed length checked")),
            sample_position: u64::from_le_bytes(
                bytes[16..24].try_into().expect("fixed length checked"),
            ),
            flags: u16::from_le_bytes(bytes[24..26].try_into().expect("fixed length checked")),
            samples: bytes[26..].try_into().expect("fixed length checked"),
        };
        block.validate()?;
        Ok(block)
    }
}
