//! First-revision native audio encoder contract.
use crate::{AudioFrame, AudioUnitSink};
use amaterasu_abi::{NativeInterface, Status};
use std::ffi::c_void;

pub const CODEC_OPUS: u32 = 2;
pub const AUDIO_PACKET_DISCONTINUITY: u32 = 1;
pub const AUDIO_PACKET_END_OF_STREAM: u32 = 2;
pub const MAX_ENCODED_AUDIO_BYTES: usize = 1024 * 1024;

#[repr(C)]
pub struct AudioEncoderApi {
    pub base: NativeInterface,
    /// STATUS_OK accepts all samples; MEDIA_BUSY accepts none. Input is borrowed
    /// for this call only. Format and continuity policy belong to the control API.
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const AudioFrame) -> Status,
    /// At most one packet callback. Rejected packets remain available for retry.
    pub receive: unsafe extern "C" fn(*mut c_void, u64, AudioUnitSink) -> Status,
    /// Stop accepting input and deliver remaining output, followed by one EOS.
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    #[test]
    fn audio_encoder_matches_public_c_layout() {
        assert_eq!(size_of::<AudioEncoderApi>(), 56);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, submit), 32);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, receive), 40);
        assert_eq!(std::mem::offset_of!(AudioEncoderApi, drain), 48);
    }
}
