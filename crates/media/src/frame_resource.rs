//! Negotiated capture resources. These are trusted native ABI contracts, not a sandbox.
use crate::{CaptureApiV1_7, INTERFACE_MAJOR};
use amaterasu_abi::{AbiHeader, NativeInterface, Status};
use std::{ffi::c_void, mem::size_of};

pub const CAPTURE_RESOURCE_INTERFACE_MINOR: u32 = 8;
pub const FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR: u32 = 1;
pub const FRAME_RESOURCE_CONSUMER_INTERFACE_MINOR: u32 = 0;
pub const FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR: u32 = 1;
pub const FRAME_RESOURCE_PROCESSOR_INTERFACE_MINOR: u32 = 0;
pub const FRAME_RESOURCE_CPU: u32 = 1;
pub const FRAME_RESOURCE_DMABUF: u32 = 2;
/// Resource and synchronization kinds >= this value require an independently
/// agreed vendor schema. Unknown kinds must never be interpreted as known ones.
pub const FRAME_RESOURCE_VENDOR_BASE: u32 = 0x8000_0000;
pub const FRAME_SYNC_NONE: u32 = 0;
/// Linux sync_file: descriptor v1 points to `FrameSyncFile`.
pub const FRAME_SYNC_SYNC_FILE: u32 = 1;
pub const FRAME_SYNC_VENDOR_BASE: u32 = 0x8000_0000;

/// A typed, versioned descriptor. The schema is determined by kind and version;
/// size is in bytes. Pointers and any handles within are borrowed. NONE has
/// version=0, size=0, namespace all zero and data=null. Unknown versions must be rejected.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameDescriptor {
    pub kind: u32,
    pub version: u32,
    pub size: u32,
    pub reserved: u32,
    /// All zero for standard kinds; a stable nonzero UUID for vendor kinds.
    pub namespace: [u8; 16],
    pub data: *const c_void,
}

/// Borrowed Linux sync_file fd. A completion fence covers all work performed
/// under the released reference. The provider duplicates it during release.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameSyncFile {
    pub fd: i32,
}

/// CPU v1 offer payload. A separate offer advertises each format.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuResourceCapabilities {
    pub format: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
}

/// DMA-BUF v1 offer payload: one format/modifier/color tuple and size range.
/// `device_id` is an agreed 16-byte device UUID; zero means unknown, not a
/// wildcard proving interoperability. Consumer configure must check importability.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DmaBufResourceCapabilities {
    pub fourcc: u32,
    pub color: u32,
    pub modifier: u64,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
    pub device_id: [u8; 16],
}

pub const FRAME_OWNERSHIP_BORROWED: u32 = 1;
pub const FRAME_OWNERSHIP_RETAINABLE: u32 = 2;
/// Resource schema exposes a direct CPU access mechanism to the consumer.
pub const FRAME_ACCESS_CPU: u32 = 1;
/// Resource schema exposes a direct GPU access/import mechanism to the consumer.
pub const FRAME_ACCESS_GPU: u32 = 2;

/// One provider offer. IDs are nonzero, unique within a generation and stream.
/// Enumeration reports a consistent nonzero generation, including an empty
/// result; select uses the exact generation and ID. `resource` and `acquire_sync`
/// describe the offered schemas. CPU and DMA-BUF resource offer payloads are
/// `CpuResourceCapabilities` and `DmaBufResourceCapabilities`, respectively;
/// these differ from frame payloads. Sync offer payloads are schema-specific.
/// Descriptors and pointed-to data are borrowed only during the sink callback.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureResourceOffer {
    pub id: u64,
    /// BORROWED or RETAINABLE. Borrowed-only retain returns UNSUPPORTED.
    pub ownership: u32,
    /// Bitset of FRAME_ACCESS_*. Zero means unspecified, not inaccessible.
    /// These describe access mechanisms in the resource schema, not physical
    /// residency, zero-copy, or absence of staging. Actual access remains subject
    /// to the schema's device, format, synchronization and permission constraints.
    pub accessibility: u32,
    pub reserved: u32,
    pub resource: FrameDescriptor,
    pub acquire_sync: FrameDescriptor,
    pub completion_sync: FrameDescriptor,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureResourceOfferSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const CaptureResourceOffer) -> Status,
}

/// Unknown values must remain unknown; consumers must not infer SDR or HDR.
pub const FRAME_COLOR_UNKNOWN: u32 = 0;
pub const FRAME_PRIMARIES_BT709: u32 = 1;
pub const FRAME_PRIMARIES_BT2020: u32 = 2;
pub const FRAME_TRANSFER_SRGB: u32 = 1;
pub const FRAME_TRANSFER_BT709: u32 = 2;
pub const FRAME_TRANSFER_PQ: u32 = 3;
pub const FRAME_TRANSFER_HLG: u32 = 4;
pub const FRAME_MATRIX_RGB: u32 = 1;
pub const FRAME_MATRIX_BT709: u32 = 2;
pub const FRAME_MATRIX_BT2020_NCL: u32 = 3;
pub const FRAME_RANGE_FULL: u32 = 1;
pub const FRAME_RANGE_LIMITED: u32 = 2;
pub const FRAME_CLOCK_UNKNOWN: u32 = 0;
pub const FRAME_CLOCK_STREAM_RELATIVE: u32 = 1;
pub const FRAME_CLOCK_HOST_MONOTONIC: u32 = 2;

/// Optional versioned color information. All zero means absent. Version 1 has
/// size equal to this struct; each zero component is independently unknown.
/// Unknown or unrecognized values must not be interpreted as a known color.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameColorMetadata {
    pub version: u32,
    pub size: u32,
    pub primaries: u32,
    pub transfer: u32,
    pub matrix: u32,
    pub range: u32,
}

/// Optional timestamp clock identity. All zero means absent. In v1, timestamps
/// are microseconds in the associated clock. A nonzero
/// epoch_id is an agreed unique identity for its epoch; zero means unknown.
/// Compare timestamps only for understood matching domains and matching nonzero
/// epoch IDs. Unknown versions or domains do not authorize cross-frame comparison.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameClockMetadata {
    pub version: u32,
    pub size: u32,
    pub domain: u32,
    pub reserved: u32,
    pub epoch_id: [u8; 16],
}

/// A single immutable presentation. `resource` v1 is `CpuFrame` for CPU and
/// `DmaBufFrame` for DMA-BUF; size must be at least the corresponding struct.
/// CPU uses SYNC_NONE. DMA-BUF v1 uses SYNC_NONE here and its plane acquire
/// fences; consumers must wait for all those fences. Schema-specific resources
/// specify the interpretation of acquire and completion synchronization.
///
/// The frame and descriptors are borrowed for the synchronous sink callback.
/// Borrowed-only frames return UNSUPPORTED from retain. A successful retain adds one reference and keeps storage, immutable contents,
/// descriptor data and handles valid until its matching release. Copy the frame
/// and descriptor values before returning. Failed retain acquires nothing; the
/// provider limits retained references and may return BUSY. The binding and
/// provider remain live until all retained references and their GPU work retire.
/// Selection changes or stream close do not invalidate retained references;
/// shutdown must defer provider destruction until their completion fences retire.
///
/// Each release consumes one retained reference. NONE asserts all consumer work
/// has completed. Otherwise completion describes pending work in the selected
/// synchronization schema. Its descriptor and handles are borrowed only until
/// release returns: the provider must synchronously duplicate or retain anything
/// it needs. On failure it must quarantine storage, never reuse it early. A caller
/// must not release until it has completed all accesses or supplied a valid
/// completion object covering them. No callbacks may unwind across the ABI.
/// Retain and release are invoked on the module thread; no concurrent-use promise.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureResourceFrame {
    pub offer_generation: u64,
    pub offer_id: u64,
    pub timestamp_us: u64,
    pub capture_pacing_timestamp_us: u64,
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub freshness: u32,
    pub reserved: u32,
    pub color: FrameColorMetadata,
    /// Clock for timestamp_us.
    pub clock: FrameClockMetadata,
    /// Independent clock for capture_pacing_timestamp_us.
    pub pacing_clock: FrameClockMetadata,
    pub resource: FrameDescriptor,
    pub acquire_sync: FrameDescriptor,
    pub context: *mut c_void,
    pub retain: unsafe extern "C" fn(*mut c_void) -> Status,
    pub release: unsafe extern "C" fn(*mut c_void, *const FrameDescriptor),
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CaptureResourceFrameSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const CaptureResourceFrame) -> Status,
}

/// Additive v1.8 capture table. Enumeration invokes the sink synchronously and
/// writes the generation even for an empty list. Errors propagate and output is
/// invalid on error. Selecting an unavailable/stale offer returns UNSUPPORTED
/// and leaves the previous selection unchanged. Successful selection replaces
/// it atomically. A source capability change invalidates the selection; read
/// returns UNSUPPORTED until re-enumerated and selected. Reads synchronously
/// deliver at most one frame; PENDING delivers none. A sink error propagates.
/// These operations do not change or remove the legacy frame APIs. Access uses
/// the authenticated stream owner and binding, just like legacy capture. The
/// provider owns the initial frame through the entire sink callback; a sink
/// return never implicitly transfers a reference.
#[repr(C)]
pub struct CaptureApiV1_8 {
    pub base: CaptureApiV1_7,
    pub enumerate_resources:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub select_resource: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> Status,
    pub read_resource_frame:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceFrameSink) -> Status,
}

/// Independently bound resource consumer, usable by an encoder or another sink.
/// Its offers describe accepted resource/acquire/completion schemas. IDs and
/// generation belong to this consumer stream, not the capture stream. Both
/// offer descriptors must be interpreted according to their exact kind/version
/// and capability data; equality of kind alone does not establish compatibility.
/// Configure receives the selected producer offer during the call and must check
/// actual format, device and synchronization compatibility, returning UNSUPPORTED
/// without changing its previous configuration if incompatible or stale. Caller
/// selects the producer separately and handles renegotiation if either side changes.
/// Configure must copy any offer data needed after it returns. Configure first,
/// then select the producer; if selection fails, do not submit, and reconfigure
/// after re-enumeration. Configure success alone never commits producer selection.
/// Submit receives a synchronous borrow; before returning it must finish all
/// accesses or retain a reference for each asynchronous use. Failed submit must
/// not retain work; a successful asynchronous submit releases each retained
/// reference after completion or with the negotiated completion fence. Cancel
/// stops admission and retires all retained references and pending work before
/// returning. The owning binding must stay live throughout cancellation.
#[repr(C)]
pub struct FrameResourceConsumerApi {
    pub base: NativeInterface,
    pub enumerate_resources:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub configure: unsafe extern "C" fn(
        *mut c_void,
        u64,
        u64,
        u64,
        u64,
        *const CaptureResourceOffer,
    ) -> Status,
    pub submit: unsafe extern "C" fn(*mut c_void, u64, *const CaptureResourceFrame, u32) -> Status,
    pub cancel: unsafe extern "C" fn(*mut c_void, u64),
}

/// Resource processor extends the consumer contract. Process may synchronously
/// deliver at most one borrowed output frame; the sink may retain it. Input
/// lifetime and cancellation obey the consumer contract. Output frames use
/// the same descriptor and retain/release rules, and preserve source sequence,
/// timestamps and freshness. The output offer is independently negotiated;
/// process never invokes the sink after returning.
#[repr(C)]
pub struct FrameResourceProcessorApi {
    pub base: FrameResourceConsumerApi,
    pub enumerate_outputs:
        unsafe extern "C" fn(*mut c_void, u64, CaptureResourceOfferSink, *mut u64) -> Status,
    pub select_output: unsafe extern "C" fn(*mut c_void, u64, u64, u64) -> Status,
    pub process: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const CaptureResourceFrame,
        CaptureResourceFrameSink,
    ) -> Status,
}

pub const FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE: u32 = size_of::<FrameResourceConsumerApi>() as u32;
pub const FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE: u32 =
    size_of::<FrameResourceProcessorApi>() as u32;

pub const CAPTURE_API_V1_8_SIZE: u32 = size_of::<CaptureApiV1_8>() as u32;

pub fn capture_resources_available(header: AbiHeader) -> bool {
    header.major == INTERFACE_MAJOR
        && header.minor >= CAPTURE_RESOURCE_INTERFACE_MINOR
        && header.size >= CAPTURE_API_V1_8_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn c_layouts_match_public_header() {
        assert_eq!(size_of::<FrameDescriptor>(), 40);
        assert_eq!(size_of::<FrameColorMetadata>(), 24);
        assert_eq!(size_of::<FrameClockMetadata>(), 32);
        assert_eq!(size_of::<CaptureResourceOffer>(), 144);
        assert_eq!(size_of::<CaptureResourceFrame>(), 248);
        assert_eq!(std::mem::offset_of!(CaptureResourceFrame, resource), 144);
        assert_eq!(std::mem::offset_of!(FrameResourceProcessorApi, base), 0);
    }

    #[test]
    fn version_gate_and_capture_prefix() {
        assert_eq!(std::mem::offset_of!(CaptureApiV1_8, base), 0);
        assert_eq!(
            std::mem::offset_of!(CaptureApiV1_8, enumerate_resources),
            size_of::<CaptureApiV1_7>()
        );
        let valid = AbiHeader {
            major: INTERFACE_MAJOR,
            minor: CAPTURE_RESOURCE_INTERFACE_MINOR,
            size: CAPTURE_API_V1_8_SIZE,
        };
        assert!(capture_resources_available(valid));
        assert!(!capture_resources_available(AbiHeader {
            minor: 7,
            ..valid
        }));
        assert!(!capture_resources_available(AbiHeader {
            size: CAPTURE_API_V1_8_SIZE - 1,
            ..valid
        }));
        assert!(!capture_resources_available(AbiHeader {
            major: 2,
            ..valid
        }));
    }
}
