//! Internal decoded-sample view used by the GameStream Opus packetizer.
use amaterasu_media::{
    AUDIO_PACKET_DISCONTINUITY, AUDIO_PACKET_END_OF_STREAM, MAX_ENCODED_AUDIO_BYTES,
};
use amaterasu_module_sdk::abi::{STATUS_INVALID_ARGUMENT, Status};

/// `frames` counts decoded samples per
/// channel, and `timestamp_us` identifies the first sample relative to the stream
/// epoch. Channel ordering and multistream mapping belong to the control contract.
/// Discontinuity marks a codec/continuity reset; EOS contains no samples or bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OpusPacket {
    pub codec: u32,
    pub flags: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub frames: u32,
    pub data: *const u8,
    pub len: usize,
    pub timestamp_us: u64,
    pub sequence: u64,
}

impl OpusPacket {
    /// Validates bounded metadata, not packet syntax or pointer validity.
    pub fn validate(&self) -> Result<(), Status> {
        if self.codec == 0
            || self.sample_rate == 0
            || self.channels == 0
            || self.flags & !(AUDIO_PACKET_DISCONTINUITY | AUDIO_PACKET_END_OF_STREAM) != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.flags & AUDIO_PACKET_END_OF_STREAM != 0 {
            if self.flags != AUDIO_PACKET_END_OF_STREAM || self.frames != 0 || self.len != 0 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if self.frames == 0
            || self.len == 0
            || self.len > MAX_ENCODED_AUDIO_BYTES
            || self.data.is_null()
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// Nonempty data must contain `len` initialized bytes and remain alive and
    /// immutable for the returned borrow. Validation does not establish this.
    pub unsafe fn bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: allocation validity and borrow lifetime are caller obligations.
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.len) })
    }
}
