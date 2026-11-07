//! Optional writable encoder-owned input targets. Native modules are trusted code.
use crate::{
    CaptureResourceOffer, CaptureResourceOfferSink, FrameClockMetadata, FrameColorMetadata,
    FrameDescriptor,
};
use amaterasu_abi::{AbiHeader, NativeInterface, Status};
use std::{ffi::c_void, mem::size_of};

/// Standard pool-only writable CPU resource, distinct from immutable CPU frames.
/// Its `FrameDescriptor` has version 1, zero namespace and the payload type
/// specified by the context: capabilities in offers, writable frame in acquire.
pub const ENCODER_INPUT_RESOURCE_CPU_WRITABLE: u32 = 3;

/// One supported CPU pixel format and dimensions. Alignment fields are nonzero
/// powers of two in bytes. Only BGRA8 and NV12 have defined v1 layouts. CPU
/// resource, acquire and writer completion use NONE synchronization; CPU writes
/// must be complete before commit or abandon. Device identity is not applicable.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputCpuWritableCapabilities {
    pub format: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub stride_alignment: u32,
    pub data_alignment: u32,
}

/// Mutable bytes owned by the provider, exclusively borrowed for one acquired
/// lease. `len` is capacity in bytes, not necessarily the used image size. The
/// address is aligned as negotiated; padding is part of the writable allocation.
/// Wait for acquire before writing and cease all access before a successful
/// commit or abandon. No other references may access the allocation while it is
/// mutably borrowed. A source read must not overlap this target. The pointer is
/// never owned or freed by the writer and cannot be retained after lease end.
/// Validating metadata cannot prove allocation validity or exclusive access.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputCpuWritableFrame {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride: u32,
    pub data: *mut u8,
    pub len: usize,
}

pub fn encoder_input_cpu_writable_capabilities_valid(
    caps: &EncoderInputCpuWritableCapabilities,
) -> bool {
    matches!(
        caps.format,
        crate::PIXEL_BGRA8 | crate::PIXEL_NV12 | crate::PIXEL_P010
    ) && caps.min_width != 0
        && caps.min_width <= caps.max_width
        && caps.max_width <= 8192
        && caps.min_height != 0
        && caps.min_height <= caps.max_height
        && caps.max_height <= 8192
        && caps.stride_alignment.is_power_of_two()
        && caps.data_alignment.is_power_of_two()
}

/// Checks geometry, bounded capacity and negotiated alignment, without
/// dereferencing `data` or proving pointer validity or non-aliasing.
pub fn encoder_input_cpu_writable_valid(
    frame: &EncoderInputCpuWritableFrame,
    caps: &EncoderInputCpuWritableCapabilities,
) -> bool {
    encoder_input_cpu_writable_capabilities_valid(caps)
        && frame.format == caps.format
        && (caps.min_width..=caps.max_width).contains(&frame.width)
        && (caps.min_height..=caps.max_height).contains(&frame.height)
        && frame.stride.is_multiple_of(caps.stride_alignment)
        && !frame.data.is_null()
        && (frame.data as usize).is_multiple_of(caps.data_alignment as usize)
        && frame.len <= crate::MAX_FRAME_BYTES
        && crate::frame_size(frame.width, frame.height, frame.format, frame.stride)
            .is_ok_and(|required| frame.len >= required)
}

pub const ENCODER_INPUT_POOL_INTERFACE_MAJOR: u32 = 1;
pub const ENCODER_INPUT_POOL_INTERFACE_MINOR: u32 = 0;
pub const ENCODER_INPUT_POOL_MAX_TARGETS: u32 = 64;
pub const ENCODER_INPUT_POOL_RETIRED: u32 = 1;
pub const ENCODER_INPUT_POOL_QUARANTINED: u32 = 2;

/// Selection of one enumerated offer and bounded pool geometry. The encoder
/// validates the complete offer schema, device identity, dimensions and sync
/// compatibility, not merely descriptor kinds. Zero device UUID is unknown.
/// Configure is atomic on failure. A successful configure creates a fresh,
/// nonzero generation even if dimensions are unchanged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputPoolConfig {
    pub version: u32,
    pub size: u32,
    pub offer_generation: u64,
    pub offer_id: u64,
    pub width: u32,
    pub height: u32,
    pub target_count: u32,
    pub reserved: u32,
}

/// A lease for a writable target. generation, target_id and lease_id are
/// nonzero; target_id is unique in its generation and lease_id is never reused
/// within a stream. Descriptors and handles stay borrowed and valid until the
/// lease is consumed. The writer must wait on acquire_sync before any access;
/// resource-specific plane fences also apply. A NONE acquire asserts previous
/// encoder reads have already completed. No thread-safety is implied.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputTarget {
    pub generation: u64,
    pub target_id: u64,
    pub lease_id: u64,
    pub resource: FrameDescriptor,
    pub acquire_sync: FrameDescriptor,
}

/// Submission metadata and writer completion. When frame options are prepared,
/// control_id and sequence both equal the nonzero EncoderFrameOptionsV1
/// submission_id; session, source epoch, timestamp and clock must match that
/// preparation. Otherwise control_id is zero. A rejected commit preserves the
/// preparation for retry of the same frame and metadata, or cancellation.
/// The writer must finish all accesses or provide a valid completion fence.
/// NONE asserts completion. The encoder duplicates any needed fence data and
/// handles before returning success and waits for completion before reading.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputSubmission {
    pub version: u32,
    pub size: u32,
    pub generation: u64,
    pub target_id: u64,
    pub lease_id: u64,
    pub control_id: u64,
    pub timestamp_us: u64,
    pub sequence: u64,
    pub color: FrameColorMetadata,
    pub clock: FrameClockMetadata,
    pub completion_sync: FrameDescriptor,
}

/// A terminal retirement query: RETIRED means all accesses are known complete;
/// QUARANTINED means completion cannot be established and storage must remain
/// isolated. Neither outcome permits pretending uncertain GPU work completed.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderInputPoolRetirement {
    pub version: u32,
    pub size: u32,
    pub generation: u64,
    pub state: u32,
    pub reserved: u32,
}

/// A separate signed native service `vendor.encode-pool/1`, bound to the owning
/// encoder provider's authenticated principal and session namespace. A native
/// query cannot distinguish this table from other major-1 tables by type. Pool
/// and encoder bindings share session identity and must remain alive as required
/// by their resources; the pool is not another view of the provider table.
/// Offers use CaptureResourceOffer and its descriptor schemas. Enumeration is
/// synchronous; generation is nonzero even for an empty set. The offer's resource
/// describes writable target capability; acquire_sync describes encoder-to-writer
/// synchronization, completion_sync writer-to-encoder. Schema compatibility
/// includes namespace, version, device, format and synchronization semantics.
///
/// Acquire returns one exclusive lease or BUSY without creating one. Successful
/// commit consumes the lease exactly once and queues encoding; any failure
/// consumes nothing, schedules no read and returns ownership to the writer.
/// Abandon consumes exactly one lease only on success; its completion descriptor
/// covers all writer accesses, including asynchronous work. Failed abandon leaves
/// ownership with the writer. Reject stale, duplicate, wrong-stream and consumed
/// identities. Never reuse a target until all writer and encoder accesses safely
/// complete. Invalid or failed fences require quarantine, never silent reuse.
///
/// Reconfigure stops old acquisitions; old leases can be abandoned, but stale
/// commits fail without consuming them. Begin retirement stops all new acquire,
/// configure and commit admission on success; existing leases must still be
/// abandoned with completion. It may return an error without changing admission.
/// No concurrent calls are permitted. Retirement returns PENDING while work or
/// leases remain, RETIRED only when all accesses are proven complete, or
/// QUARANTINED when completion is uncertain. Until RETIRED, the pool and encoder
/// bindings, code and all source, device and storage resources must remain live
/// until independently proven safe. Process exit ends in-process ownership but
/// does not prove external GPU, DMA or shared-handle access has stopped; safe
/// termination requires proven completion or platform/driver cleanup guarantees.
/// A timeout or unknown GPU state is never proof. Quarantine can become RETIRED
/// only on actual proof.
/// This optional protocol does not alter existing synchronous cancel contracts.
/// Callbacks must not unwind across the ABI; no thread safety is implied.
#[repr(C)]
pub struct EncoderInputPoolApiV1 {
    pub base: NativeInterface,
    pub enumerate:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub configure:
        unsafe extern "C" fn(*mut c_void, u64, *const EncoderInputPoolConfig, *mut u64) -> Status,
    pub acquire: unsafe extern "C" fn(*mut c_void, u64, u64, *mut EncoderInputTarget) -> Status,
    pub commit: unsafe extern "C" fn(*mut c_void, u64, *const EncoderInputSubmission) -> Status,
    pub abandon:
        unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, *const FrameDescriptor) -> Status,
    pub begin_retirement: unsafe extern "C" fn(*mut c_void, u64) -> Status,
    pub retirement:
        unsafe extern "C" fn(*mut c_void, u64, u64, *mut EncoderInputPoolRetirement) -> Status,
}

pub const ENCODER_INPUT_POOL_API_V1_SIZE: u32 = size_of::<EncoderInputPoolApiV1>() as u32;

pub fn encoder_input_pool_available(header: AbiHeader) -> bool {
    header.major == ENCODER_INPUT_POOL_INTERFACE_MAJOR
        && header.size >= ENCODER_INPUT_POOL_API_V1_SIZE
}

/// Basic structural checks; the provider must additionally validate offer,
/// device, dimensions and live generation against its own state.
pub fn encoder_input_pool_config_valid(config: &EncoderInputPoolConfig) -> bool {
    config.version == 1
        && config.size >= size_of::<EncoderInputPoolConfig>() as u32
        && config.offer_generation != 0
        && config.offer_id != 0
        && config.width != 0
        && config.height != 0
        && (1..=ENCODER_INPUT_POOL_MAX_TARGETS).contains(&config.target_count)
        && config.reserved == 0
}

/// The offer type is shared with other resource negotiation interfaces.
pub type EncoderInputPoolOffer = CaptureResourceOffer;
pub type EncoderInputPoolOfferSink = CaptureResourceOfferSink;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writable_cpu_checks_format_capacity_bounds_and_alignment() {
        let caps = EncoderInputCpuWritableCapabilities {
            format: crate::PIXEL_BGRA8,
            min_width: 1,
            max_width: 8192,
            min_height: 1,
            max_height: 8192,
            stride_alignment: 4,
            data_alignment: 4,
        };
        let mut pixels = [0u32; 4];
        let mut frame = EncoderInputCpuWritableFrame {
            width: 2,
            height: 2,
            format: crate::PIXEL_BGRA8,
            stride: 8,
            data: pixels.as_mut_ptr().cast(),
            len: 16,
        };
        assert!(encoder_input_cpu_writable_valid(&frame, &caps));
        frame.len = 15;
        assert!(!encoder_input_cpu_writable_valid(&frame, &caps));
        frame.len = 16;
        frame.stride = 7;
        assert!(!encoder_input_cpu_writable_valid(&frame, &caps));
        frame.stride = 8;
        frame.data = unsafe { frame.data.add(1) };
        assert!(!encoder_input_cpu_writable_valid(&frame, &caps));
        frame.data = pixels.as_mut_ptr().cast();
        frame.format = 999;
        assert!(!encoder_input_cpu_writable_valid(&frame, &caps));
        frame.format = crate::PIXEL_BGRA8;
        frame.width = u32::MAX;
        assert!(!encoder_input_cpu_writable_valid(&frame, &caps));
        let nv12 = EncoderInputCpuWritableCapabilities {
            format: crate::PIXEL_NV12,
            ..caps
        };
        frame.width = 1;
        frame.format = crate::PIXEL_NV12;
        assert!(!encoder_input_cpu_writable_valid(&frame, &nv12));
        let p010 = EncoderInputCpuWritableCapabilities {
            format: crate::PIXEL_P010,
            ..caps
        };
        assert!(encoder_input_cpu_writable_capabilities_valid(&p010));
        frame.width = 2;
        frame.format = crate::PIXEL_P010;
        frame.stride = 4;
        frame.len = 12;
        assert!(encoder_input_cpu_writable_valid(&frame, &p010));
        frame.stride = 5;
        assert!(!encoder_input_cpu_writable_valid(&frame, &p010));
    }

    #[test]
    fn rejects_bad_config_and_version() {
        let mut config = EncoderInputPoolConfig {
            version: 1,
            size: size_of::<EncoderInputPoolConfig>() as u32,
            offer_generation: 1,
            offer_id: 1,
            width: 1920,
            height: 1080,
            target_count: 2,
            reserved: 0,
        };
        assert!(encoder_input_pool_config_valid(&config));
        config.target_count = ENCODER_INPUT_POOL_MAX_TARGETS + 1;
        assert!(!encoder_input_pool_config_valid(&config));
        config.target_count = 0;
        assert!(!encoder_input_pool_config_valid(&config));
        config.target_count = 1;
        config.offer_generation = 0;
        assert!(!encoder_input_pool_config_valid(&config));
        let header = AbiHeader {
            major: 1,
            minor: 0,
            size: ENCODER_INPUT_POOL_API_V1_SIZE,
        };
        assert!(encoder_input_pool_available(header));
        assert!(!encoder_input_pool_available(AbiHeader {
            size: ENCODER_INPUT_POOL_API_V1_SIZE - 1,
            ..header
        }));
        assert!(!encoder_input_pool_available(AbiHeader {
            major: 2,
            ..header
        }));
    }
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn c_layouts_match() {
        assert_eq!(size_of::<EncoderInputCpuWritableCapabilities>(), 28);
        assert_eq!(size_of::<EncoderInputCpuWritableFrame>(), 32);
        assert_eq!(size_of::<EncoderInputPoolConfig>(), 40);
        assert_eq!(size_of::<EncoderInputTarget>(), 104);
        assert_eq!(size_of::<EncoderInputSubmission>(), 152);
        assert_eq!(size_of::<EncoderInputPoolRetirement>(), 24);
        assert_eq!(size_of::<EncoderInputPoolApiV1>(), 88);
        assert_eq!(
            std::mem::offset_of!(EncoderInputPoolApiV1, begin_retirement),
            72
        );
        assert_eq!(std::mem::offset_of!(EncoderInputPoolApiV1, enumerate), 32);
        assert_eq!(
            std::mem::offset_of!(EncoderInputSubmission, completion_sync),
            112
        );
    }
}
