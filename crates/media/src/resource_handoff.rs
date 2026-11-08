//! Encoder session operations over the common frame-resource contract.
use crate::{CaptureResourceFrame, CaptureResourceOffer, CaptureResourceOfferSink, EncoderApiV1_4};
use amaterasu_abi::Status;
use std::{ffi::c_void, mem::size_of};
pub const ENCODER_RESOURCE_INTERFACE_MINOR: u32 = 5;

/// Encoder v1.5: the frame-resource consumer operations for an encoder
/// session. `configure_resource` checks the producer offer against its own
/// offer exactly as `FrameResourceConsumerApi::configure`. `submit_resource`
/// accepts a frame of the configured schema: OK retains a reference for
/// asynchronous work and releases it only once that work completed (or with
/// the negotiated completion object); BUSY and errors retain nothing and have
/// retired any GPU access they started. FORCE_IDR and unknown flags follow
/// `FRAME_RESOURCE_SUBMIT_FORCE_IDR`. `cancel_dmabuf_leases` also retires every
/// retained resource reference before returning.
#[repr(C)]
pub struct EncoderApiV1_5 {
    pub base: EncoderApiV1_4,
    pub enumerate_resources:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub configure_resource: unsafe extern "C" fn(
        *mut c_void,
        u64,
        u64,
        u64,
        u64,
        *const CaptureResourceOffer,
    ) -> Status,
    pub submit_resource:
        unsafe extern "C" fn(*mut c_void, u64, *const CaptureResourceFrame, u32) -> Status,
}
pub const ENCODER_API_V1_5_SIZE: u32 = size_of::<EncoderApiV1_5>() as u32;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoder_prefix_is_preserved() {
        assert_eq!(
            std::mem::offset_of!(EncoderApiV1_5, enumerate_resources) as u32,
            crate::ENCODER_API_V1_4_SIZE
        );
        assert_eq!(ENCODER_API_V1_5_SIZE, crate::ENCODER_API_V1_4_SIZE + 24);
    }
}
