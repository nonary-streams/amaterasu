//! Capture-independent processing over negotiated resources and GPU dependencies.
use crate::{FrameResourceProcessorApi, HdrMetadata};
use amaterasu_abi::Status;
use std::{ffi::c_void, mem::size_of};

pub const FRAME_PROCESSOR_INTERFACE_MAJOR: u32 = 1;
pub const FRAME_PROCESSOR_INTERFACE_MINOR: u32 = 0;
pub const FRAME_PROCESSOR_MAX_CONFIGURATION_BYTES: usize = 16 * 1024;

/// The complete processing contract. Its input and output are frame resources,
/// never a particular capture method, platform handle, or storage allocation.
/// The resource processor prefix supplies input/output negotiation and process.
/// A process call offers its output immediately with an acquire dependency;
/// it does not wait for GPU completion. Providers retain inputs independently
/// through queued reads and bound their allocations and retained references.
/// Configuration and updates are atomic; failed updates preserve prior settings.
/// HDR metadata describes output, independently of source metadata.
/// Bind under amaterasu.frame-processor.native/1; layout alone is not identity.
#[repr(C)]
pub struct FrameProcessorApi {
    pub base: FrameResourceProcessorApi,
    pub configure_stream: unsafe extern "C" fn(*mut c_void, u64, *const u8, usize) -> Status,
    pub update_stream: unsafe extern "C" fn(*mut c_void, u64, *const u8, usize) -> Status,
    pub hdr_metadata: unsafe extern "C" fn(*mut c_void, u64, *mut HdrMetadata) -> Status,
}
pub const FRAME_PROCESSOR_API_SIZE: u32 = size_of::<FrameProcessorApi>() as u32;
