//! Controller attributes carried by an optional, versioned input extension.
//!
//! The fields come from Vibeshine `eeab9d96c4db45de511022d451810dd2c76510f3`,
//! `src/steam_controller_protocol.h:118-154`. This SDK payload is not the
//! GameStream packet: the authenticated route supplies the controller identity.
//! Serial fields may identify hardware; callers should not log or persist them.

use super::{Capability, Direction};

pub const SCHEMA: &str = "nonary.steam-controller.attributes";
pub const PAYLOAD_BYTES: usize = 74;

pub fn capability() -> Capability {
    Capability {
        schema: SCHEMA.into(),
        version: 1,
        direction: Direction::ToDevice,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Attributes {
    /// 0 = USB, 1 = BLE, 2 = puck; unknown source values are preserved.
    pub transport: u8,
    pub valid_mask: u8,
    /// Unix seconds; only this timestamp's unit is established by the source.
    pub firmware_build_time: u32,
    pub bootloader_build_time: u32,
    pub board_revision: u32,
    pub capabilities: u32,
    pub radio_firmware_build_time: u32,
    pub dongle_firmware_build_time: u32,
    /// Fixed-width source bytes; consumers apply their own safe string handling.
    pub build_id: [u8; 16],
    pub unit_serial: [u8; 16],
    pub board_serial: [u8; 16],
}

impl Attributes {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let mut bytes = Vec::with_capacity(PAYLOAD_BYTES);
        bytes.extend_from_slice(&[self.transport, self.valid_mask]);
        for value in [
            self.firmware_build_time,
            self.bootloader_build_time,
            self.board_revision,
            self.capabilities,
            self.radio_firmware_build_time,
            self.dongle_firmware_build_time,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&self.build_id);
        bytes.extend_from_slice(&self.unit_serial);
        bytes.extend_from_slice(&self.board_serial);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != PAYLOAD_BYTES {
            return Err("invalid_attributes_length");
        }
        let read = |offset: usize| {
            u32::from_le_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("fixed length checked"),
            )
        };
        Ok(Self {
            transport: bytes[0],
            valid_mask: bytes[1],
            firmware_build_time: read(2),
            bootloader_build_time: read(6),
            board_revision: read(10),
            capabilities: read(14),
            radio_firmware_build_time: read(18),
            dongle_firmware_build_time: read(22),
            build_id: bytes[26..42].try_into().expect("fixed length checked"),
            unit_serial: bytes[42..58].try_into().expect("fixed length checked"),
            board_serial: bytes[58..74].try_into().expect("fixed length checked"),
        })
    }
}
