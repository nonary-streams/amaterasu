//! Negotiated resource handoff between a GPU processor and an encoder.
//!
//! These additive tails let the video consumer pass a processor's output to
//! the encoder in any resource schema both negotiate (for example CUDA device
//! memory), instead of a DMA-BUF lease. Offers, configuration, frames and
//! retain/release follow the frame-resource rules in `frame_resource`. When
//! either side lacks a tail, or no offer pair is compatible, the consumer keeps
//! the lease path.
use crate::{
    CaptureResourceFrame, CaptureResourceFrameSink, CaptureResourceOffer, CaptureResourceOfferSink,
    DmaBufFrameLease, EncoderApiV1_4, GpuProcessorApiV1_3,
};
use amaterasu_abi::Status;
use std::{ffi::c_void, mem::size_of};

pub const GPU_PROCESSOR_RESOURCE_OUTPUT_INTERFACE_MINOR: u32 = 4;

/// GPU processor v1.4. Output offers belong to the configured stream; their
/// generation and IDs follow the frame-resource enumeration rules. Selecting
/// an offer switches `process_to_resource` to that schema; until a selection
/// succeeds it returns UNSUPPORTED. `process_to_resource` takes the same
/// borrowed input lease and freshness as `process`, delivers at most one
/// output frame synchronously (preserving sequence, timestamps and freshness),
/// and propagates sink rejection unchanged. The sink may retain the output;
/// every retained reference stays valid until released, including after the
/// stream is cancelled, and the provider stays loaded until then. Calls are
/// serialized with configuration, updates and cancellation.
#[repr(C)]
pub struct GpuProcessorApiV1_4 {
    pub base: GpuProcessorApiV1_3,
    pub enumerate_outputs:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub select_output: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> Status,
    pub process_to_resource: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const DmaBufFrameLease,
        u32,
        CaptureResourceFrameSink,
    ) -> Status,
}
pub const GPU_PROCESSOR_API_V1_4_SIZE: u32 = size_of::<GpuProcessorApiV1_4>() as u32;

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
    use crate::{ENCODER_API_V1_4_SIZE, GPU_PROCESSOR_API_V1_3_SIZE};

    #[test]
    fn tails_preserve_their_prefixes() {
        assert_eq!(
            std::mem::offset_of!(GpuProcessorApiV1_4, enumerate_outputs) as u32,
            GPU_PROCESSOR_API_V1_3_SIZE
        );
        assert_eq!(
            std::mem::offset_of!(EncoderApiV1_5, enumerate_resources) as u32,
            ENCODER_API_V1_4_SIZE
        );
        assert_eq!(
            GPU_PROCESSOR_API_V1_4_SIZE,
            GPU_PROCESSOR_API_V1_3_SIZE + 24
        );
        assert_eq!(ENCODER_API_V1_5_SIZE, ENCODER_API_V1_4_SIZE + 24);
    }
}
