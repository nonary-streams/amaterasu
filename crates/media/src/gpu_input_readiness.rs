//! Optional advisory readiness for an encoder's leased DMA-BUF input.
use crate::EncoderApiV1_3;
use amaterasu_abi::Status;
use std::{ffi::c_void, mem::size_of};

pub const ENCODER_GPU_READINESS_INTERFACE_MINOR: u32 = 4;

/// Additive encoder tail. OK says a leased GPU input may be attempted; BUSY or
/// PENDING says defer it. UNSUPPORTED means readiness is unavailable and the
/// consumer should use ordinary submission. A query is advisory, reserves no
/// capacity, retains no frames and must not consume encoded output. Submission
/// can still return BUSY or reject an incompatible frame after an OK query.
/// The provider authorizes the session against the binding's caller.
#[repr(C)]
pub struct EncoderApiV1_4 {
    pub base: EncoderApiV1_3,
    pub gpu_input_ready: unsafe extern "C" fn(*mut c_void, u64) -> Status,
}

pub const ENCODER_API_V1_4_SIZE: u32 = size_of::<EncoderApiV1_4>() as u32;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_tail_preserves_the_v1_3_prefix() {
        assert_eq!(
            std::mem::offset_of!(EncoderApiV1_4, gpu_input_ready),
            size_of::<EncoderApiV1_3>()
        );
    }
}
