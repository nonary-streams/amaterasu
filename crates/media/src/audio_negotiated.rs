//! Separately versioned negotiated audio encoder interface. Legacy Opus is unchanged.
use crate::{
    AUDIO_PACKET_DISCONTINUITY, AUDIO_PACKET_END_OF_STREAM, AudioFrame, MAX_ENCODED_AUDIO_BYTES,
};
use amaterasu_abi::{BorrowedBytes, NativeInterface, STATUS_INVALID_ARGUMENT, Status};
use std::{ffi::c_void, mem::size_of};

pub const AUDIO_FORMAT_VERSION: u32 = 1;
pub const AUDIO_FORMAT_MAX_CONFIG_BYTES: usize = 4096;
pub const AUDIO_FORMAT_MAX_OFFERS: usize = 32;
pub const AUDIO_CODEC_OPUS: u32 = 2;
pub const AUDIO_CODEC_AC3: u32 = 3;
pub const AUDIO_CODEC_EAC3: u32 = 4;
pub const AUDIO_FRAMING_OPUS_PACKET: u32 = 1;
pub const AUDIO_FRAMING_AC3_SYNCFRAME: u32 = 2;
pub const AUDIO_FRAMING_EAC3_SYNCFRAME: u32 = 3;

/// One exact output format. All-zero namespace identifies standard codecs; a
/// nonzero namespace identifies a separately agreed vendor schema. Config bytes
/// and this descriptor are borrowed for a synchronous call only. The channel
/// mask uses speaker bits defined by the negotiated schema, not platform order.
/// Structural validity never implies that a consumer supports the codec.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatDescriptor {
    pub size: u32,
    pub version: u32,
    pub namespace: [u8; 16],
    pub codec: u32,
    pub framing: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_mask: u64,
    pub frame_samples: u32,
    pub reserved: u32,
    pub config: BorrowedBytes,
}

impl AudioFormatDescriptor {
    pub fn validate(&self) -> Result<(), Status> {
        if self.size as usize != size_of::<Self>()
            || self.version != AUDIO_FORMAT_VERSION
            || self.codec == 0
            || self.framing == 0
            || !(8_000..=192_000).contains(&self.sample_rate)
            || !(1..=8).contains(&self.channels)
            || self.channel_mask.count_ones() != self.channels
            || self.frame_samples == 0
            || self.frame_samples > self.sample_rate
            || self.reserved != 0
            || self.config.len > AUDIO_FORMAT_MAX_CONFIG_BYTES
            || (self.config.len != 0 && self.config.ptr.is_null())
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }

    /// # Safety
    /// Nonempty config must point to initialized immutable bytes for this borrow.
    pub unsafe fn config_bytes(&self) -> Result<&[u8], Status> {
        self.validate()?;
        if self.config.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: the caller guarantees the allocation and lifetime.
        Ok(unsafe { std::slice::from_raw_parts(self.config.ptr, self.config.len) })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatOffer {
    pub id: u64,
    pub format: AudioFormatDescriptor,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioFormatOfferSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const AudioFormatOffer) -> Status,
}

/// One complete encoded unit. Generation and offer identify the exact selected
/// format within the authenticated stream. Timestamps are stream-relative
/// microseconds at the first decoded sample. The sender owns bytes until the synchronous sink callback returns;
/// rejection retains the same immutable packet for retry. EOS has no bytes or
/// samples. The consumer checks the selected format before interpreting bytes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NegotiatedAudioPacket {
    pub stream: u64,
    pub generation: u64,
    pub offer_id: u64,
    pub flags: u32,
    pub frames: u32,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub data: BorrowedBytes,
}

impl NegotiatedAudioPacket {
    pub fn validate_for(
        &self,
        stream: u64,
        generation: u64,
        offer: u64,
        format: &AudioFormatDescriptor,
    ) -> Result<(), Status> {
        format.validate()?;
        if stream == 0
            || self.stream != stream
            || generation == 0
            || offer == 0
            || self.generation != generation
            || self.offer_id != offer
            || self.flags & !(AUDIO_PACKET_DISCONTINUITY | AUDIO_PACKET_END_OF_STREAM) != 0
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if self.flags == AUDIO_PACKET_END_OF_STREAM {
            if self.frames != 0 || self.data.len != 0 {
                return Err(STATUS_INVALID_ARGUMENT);
            }
        } else if self.flags & AUDIO_PACKET_END_OF_STREAM != 0
            || self.frames != format.frame_samples
            || self.data.len == 0
            || self.data.len > MAX_ENCODED_AUDIO_BYTES
            || self.data.ptr.is_null()
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        Ok(())
    }
    /// # Safety
    /// The packet's nonempty bytes remain initialized and immutable for the borrow.
    pub unsafe fn bytes(
        &self,
        stream: u64,
        generation: u64,
        offer: u64,
        format: &AudioFormatDescriptor,
    ) -> Result<&[u8], Status> {
        self.validate_for(stream, generation, offer, format)?;
        if self.data.len == 0 {
            return Ok(&[]);
        }
        // SAFETY: the caller guarantees a live immutable allocation.
        Ok(unsafe { std::slice::from_raw_parts(self.data.ptr, self.data.len) })
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NegotiatedAudioPacketSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const NegotiatedAudioPacket) -> Status,
}

/// Independent native interface family, not an extension of AudioEncoderApi.
/// enumerate returns a consistent nonzero generation and bounded unique offers;
/// configure selects only an exact offered id in that generation, or returns
/// UNSUPPORTED without changing selection. Submit, receive and drain have the
/// legacy ownership and BUSY/EOS semantics. Destroying the owning control stream
/// retires all pending work before releasing this binding.
#[repr(C)]
pub struct NegotiatedAudioEncoderApi {
    pub base: NativeInterface,
    pub enumerate: unsafe extern "C" fn(*mut c_void, u64, AudioFormatOfferSink, *mut u64) -> Status,
    pub configure: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> Status,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const AudioFrame) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, NegotiatedAudioPacketSink) -> Status,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}
