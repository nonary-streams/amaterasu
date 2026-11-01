//! Optional frame-associated side data. Existing frame and table prefixes remain unchanged.
use crate::{
    CaptureApiV1_9, CaptureResourceFrame, FrameClockMetadata, FrameResourceConsumerApi,
    FrameResourceProcessorApi,
};
use amaterasu_abi::{BorrowedBytes, Status};
use std::{ffi::c_void, mem::size_of};

/// EncoderFrameOptionsV1.side_data_schema uses {standard namespace, this value, v1}.
pub const ENCODER_SCHEMA_FRAME_METADATA_BUNDLE: u64 = 4;
pub const FRAME_METADATA_BUNDLE_VERSION: u32 = 1;
pub const FRAME_METADATA_MAX_ITEMS: usize = 16;
pub const FRAME_METADATA_MAX_ITEM_BYTES: usize = 16 * 1024;
/// Includes the complete envelope, not just item bytes.
pub const FRAME_METADATA_MAX_BUNDLE_BYTES: usize = 64 * 1024;
pub const FRAME_METADATA_ITEM_REQUIRED: u32 = 1;
pub const FRAME_METADATA_OWNER_ENCODER: u32 = 1;
pub const FRAME_METADATA_OWNER_FRAMER: u32 = 2;
/// Raw complete ITU-T T.35 payloads, not HEVC SEI or AV1 OBU bytes.
pub const FRAME_METADATA_SCHEMA_HDR10PLUS_T35: u64 = 1;
pub const FRAME_METADATA_SCHEMA_HDR_VIVID_T35: u64 = 2;
pub const CAPTURE_METADATA_INTERFACE_MINOR: u32 = 10;
pub const FRAME_RESOURCE_CONSUMER_METADATA_MINOR: u32 = 1;
pub const FRAME_RESOURCE_PROCESSOR_METADATA_MINOR: u32 = 2;

/// Separate, sized frame wrapper. `metadata` is a pointer-free canonical AFM1
/// envelope borrowed only through the synchronous call; even when the frame's
/// retain succeeds it does not retain these bytes. Copy the complete validated
/// bundle before returning if it will be used later. The frame's own descriptor,
/// retain/release, acquire and completion contracts remain unchanged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameResourceMetadataFrameV1 {
    pub size: u32,
    pub version: u32,
    pub frame: CaptureResourceFrame,
    pub metadata: BorrowedBytes,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameResourceMetadataSinkV1 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const FrameResourceMetadataFrameV1) -> Status,
}

/// Additive capture tail. Read delivers at most one synchronous callback; a
/// sink error propagates and no metadata borrow survives the call. Version and
/// size must both cover this tail before use. Absence does not authorize
/// dropping required metadata or a silent fallback to the legacy frame path.
#[repr(C)]
pub struct CaptureApiV1_10 {
    pub base: CaptureApiV1_9,
    pub read_resource_frame_with_metadata:
        unsafe extern "C" fn(*mut c_void, u64, FrameResourceMetadataSinkV1) -> Status,
}

/// Bounded schema requirement chosen by authenticated session composition.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataSchemaRuleV1 {
    pub schema: FrameMetadataSchemaId,
    /// Only FRAME_METADATA_ITEM_REQUIRED is defined.
    pub flags: u32,
    pub reserved: u32,
}

/// Immutable context passed to the selected capture stream before metadata read.
/// The capture provider must copy the descriptor during setup. Source generation
/// is the independently negotiated capture-resource generation; it is not
/// supplied by the metadata envelope. The unused schema entries are zero.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataCaptureContextV1 {
    pub size: u32,
    pub version: u32,
    pub session_id: u64,
    pub source_id: u64,
    pub source_generation: u64,
    pub config_generation: u64,
    /// Generation returned by the selected capture capability query.
    pub capability_generation: u64,
    pub clock: FrameClockMetadata,
    pub owner: u32,
    pub schema_count: u32,
    pub schemas: [FrameMetadataSchemaRuleV1; FRAME_METADATA_MAX_ITEMS],
}

impl FrameMetadataCaptureContextV1 {
    /// Validate the bounded pointer-free descriptor before capture retains it.
    pub fn validate(&self) -> Result<(), amaterasu_abi::Status> {
        use amaterasu_abi::STATUS_INVALID_ARGUMENT;
        let clock = self.clock;
        if self.size as usize != size_of::<Self>()
            || self.version != 1
            || self.session_id == 0
            || self.source_id == 0
            || self.source_generation == 0
            || self.config_generation == 0
            || self.capability_generation == 0
            || clock.version != 1
            || clock.size as usize != size_of::<FrameClockMetadata>()
            || !matches!(
                clock.domain,
                crate::FRAME_CLOCK_HOST_MONOTONIC | crate::FRAME_CLOCK_STREAM_RELATIVE
            )
            || clock.reserved != 0
            || clock.epoch_id == [0; 16]
            || !matches!(
                self.owner,
                FRAME_METADATA_OWNER_ENCODER | FRAME_METADATA_OWNER_FRAMER
            )
            || self.schema_count == 0
            || self.schema_count as usize > FRAME_METADATA_MAX_ITEMS
        {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let mut previous = None;
        for (index, rule) in self.schemas.iter().enumerate() {
            let key = (
                rule.schema.namespace_id,
                rule.schema.value,
                rule.schema.schema,
            );
            if index >= self.schema_count as usize {
                if key != (0, 0, 0) || rule.flags != 0 || rule.reserved != 0 {
                    return Err(STATUS_INVALID_ARGUMENT);
                }
            } else {
                if key.0 == 0
                    || key.1 == 0
                    || key.2 == 0
                    || rule.reserved != 0
                    || rule.flags & !FRAME_METADATA_ITEM_REQUIRED != 0
                    || previous.is_some_and(|value| value >= key)
                {
                    return Err(STATUS_INVALID_ARGUMENT);
                }
                previous = Some(key);
            }
        }
        Ok(())
    }
}

/// Capture metadata setup tail. Setup is idempotent for identical context; a
/// conflicting setup must fail without changing the live selection. A non-OK
/// result admits nothing. Closing capture retires its setup and borrowed frames.
#[repr(C)]
pub struct CaptureApiV1_11 {
    pub base: CaptureApiV1_10,
    pub configure_frame_metadata:
        unsafe extern "C" fn(*mut c_void, u64, *const FrameMetadataCaptureContextV1) -> Status,
}
pub const CAPTURE_METADATA_CONTEXT_INTERFACE_MINOR: u32 = 11;
pub const CAPTURE_API_V1_11_SIZE: u32 = size_of::<CaptureApiV1_11>() as u32;

/// Additive consumer tail. The bound session's trusted composition selects the
/// accepted schemas and exactly one carriage owner before admission. A provider
/// cannot authorize itself by naming an owner in an envelope. OK accepts the
/// frame and metadata together; any other status consumes neither. A provider
/// retaining an asynchronous frame must separately copy the bounded envelope.
/// Cancel retires both as in the existing base contract.
#[repr(C)]
pub struct FrameResourceConsumerApiV1_1 {
    pub base: FrameResourceConsumerApi,
    pub submit_with_metadata:
        unsafe extern "C" fn(*mut c_void, u64, *const FrameResourceMetadataFrameV1, u32) -> Status,
}

/// Additive processor tail. The output frame and its envelope have the same
/// input session, submission, source/generations, timestamp and clock identity;
/// output metadata may change only under an explicit negotiated schema policy.
/// It must not silently lose required metadata or apply bitstream carriage.
/// Calls obey the v1.1 retry-safe processor rules. A successful output is a
/// synchronous borrow; its envelope never inherits a frame retain.
#[repr(C)]
pub struct FrameResourceProcessorApiV1_2 {
    pub base: FrameResourceProcessorApi,
    pub process_with_metadata: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const FrameResourceMetadataFrameV1,
        FrameResourceMetadataSinkV1,
    ) -> Status,
}

pub const FRAME_RESOURCE_METADATA_FRAME_V1_SIZE: u32 =
    size_of::<FrameResourceMetadataFrameV1>() as u32;
pub const CAPTURE_API_V1_10_SIZE: u32 = size_of::<CaptureApiV1_10>() as u32;
pub const FRAME_RESOURCE_CONSUMER_API_V1_1_SIZE: u32 =
    size_of::<FrameResourceConsumerApiV1_1>() as u32;
pub const FRAME_RESOURCE_PROCESSOR_API_V1_2_SIZE: u32 =
    size_of::<FrameResourceProcessorApiV1_2>() as u32;

/// Exact schema capability in the frame-metadata namespace. A declaration is
/// evidence for negotiation only; the host still validates provider behavior.
pub const FRAME_METADATA_CAP_FORWARD: u32 = 1;
pub const FRAME_METADATA_CAP_APPLY: u32 = 2;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataSchemaId {
    pub namespace_id: u64,
    pub value: u64,
    pub schema: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataCapabilityV1 {
    pub schema: FrameMetadataSchemaId,
    pub operations: u32,
    pub reserved: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataCapabilitySinkV1 {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const FrameMetadataCapabilityV1) -> Status,
}
/// Optional separately bound native service. A provider enumerates exact schema
/// capabilities for an authenticated session, returning one nonzero generation
/// even for no capabilities. Enumeration is synchronous, bounded and must not
/// block; the sink borrows each descriptor only through its callback. Selection
/// of owner and stage belongs to trusted startup/session composition, not this
/// provider. Capability changes invalidate dependent selections.
#[repr(C)]
pub struct FrameMetadataCapabilitiesApiV1 {
    pub base: amaterasu_abi::NativeInterface,
    pub enumerate:
        unsafe extern "C" fn(*mut c_void, u64, FrameMetadataCapabilitySinkV1, *mut u64) -> Status,
}
pub const FRAME_METADATA_CAPABILITIES_API_V1_SIZE: u32 =
    size_of::<FrameMetadataCapabilitiesApiV1>() as u32;

pub const FRAME_METADATA_STAGE_CAPTURE: u32 = 1;
pub const FRAME_METADATA_STAGE_PROCESSOR: u32 = 2;
pub const FRAME_METADATA_STAGE_ENCODER: u32 = 3;

/// Source-independent, read-only metadata preflight. The host supplies the
/// candidate profile and output geometry; this does not create provider state
/// or stand in for the per-session capability and source-generation checks.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FrameMetadataProfileProbeV1 {
    pub size: u32,
    pub version: u32,
    pub stage: u32,
    pub owner: u32,
    pub schema: FrameMetadataSchemaId,
    pub codec: crate::EncoderFormatId,
    pub framing: crate::EncoderFormatId,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bit_depth: u32,
    pub chroma: u32,
    pub reserved: u32,
    pub bitrate: u64,
}

/// Additive capability v1.1 profile probe. OK is a provider declaration,
/// not an allocation or evidence of client or hardware interoperability.
#[repr(C)]
pub struct FrameMetadataCapabilitiesApiV1_1 {
    pub base: FrameMetadataCapabilitiesApiV1,
    pub probe_profile:
        unsafe extern "C" fn(*mut c_void, *const FrameMetadataProfileProbeV1) -> Status,
}
