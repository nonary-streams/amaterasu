//! Optional encoder-provider ABI, independent of the legacy encoder tables.
use crate::{FRAME_CLOCK_STREAM_RELATIVE, FrameClockMetadata, FrameColorMetadata};
use amaterasu_abi::{AbiHeader, BorrowedBytes, NativeInterface, Status};
use std::{ffi::c_void, mem::size_of};

pub const ENCODER_PROVIDER_INTERFACE_MAJOR: u32 = 1;
pub const ENCODER_PROVIDER_INTERFACE_MINOR: u32 = 0;
pub const ENCODER_PROVIDER_MAX_OPAQUE_BYTES: usize = 64 * 1024;
pub const ENCODER_PROVIDER_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Per session, includes stored requests, results and replay tombstones.
pub const ENCODER_PROVIDER_MAX_CONTROL_RECORDS: usize = 1024;
pub const ENCODER_PROVIDER_MAX_CONTROL_BYTES: usize = 8 * 1024 * 1024;
/// Standard format IDs use namespace 1 and schema 1. Codec values reuse
/// CODEC_H264/HEVC/AV1; framing values below describe elementary stream bytes.
pub const ENCODER_FORMAT_NAMESPACE_STANDARD: u64 = 1;
pub const ENCODER_FORMAT_SCHEMA_V1: u64 = 1;
pub const ENCODER_FRAMING_ANNEX_B: u64 = 1;
pub const ENCODER_FRAMING_LENGTH_PREFIXED: u64 = 2;
pub const ENCODER_FRAMING_AV1_OBU: u64 = 3;
/// Schema IDs use the standard namespace and schema version 1.
pub const ENCODER_SCHEMA_COMMON_CONFIG: u64 = 1;
pub const ENCODER_SCHEMA_FRAME_OPTIONS: u64 = 2;
pub const ENCODER_SCHEMA_PROVIDER_LIMITS: u64 = 3;
pub const ENCODER_CONFIG_HAS_PROFILE: u32 = 1;
pub const ENCODER_CONFIG_HAS_LEVEL: u32 = 2;
pub const ENCODER_CONFIG_HAS_COLOR: u32 = 4;
pub const ENCODER_CHROMA_420: u32 = 1;
pub const ENCODER_CHROMA_422: u32 = 2;
pub const ENCODER_CHROMA_444: u32 = 3;
pub const ENCODER_CHROMA_MONOCHROME: u32 = 4;
pub const ENCODER_SESSION_SUBMITTED: u32 = 1;
pub const ENCODER_SESSION_SOURCE: u32 = 2;
pub const ENCODER_CHUNK_ACCESS_UNIT_START: u32 = 1;
pub const ENCODER_CHUNK_ACCESS_UNIT_END: u32 = 2;
pub const ENCODER_CHUNK_KEYFRAME: u32 = 4;
pub const ENCODER_CHUNK_DISCONTINUITY: u32 = 8;
pub const ENCODER_CHUNK_CORRUPT: u32 = 16;
pub const ENCODER_CHUNK_END_OF_STREAM: u32 = 32;
pub const ENCODER_CONTROL_RECONFIGURE: u32 = 1;
pub const ENCODER_CONTROL_REQUEST_KEYFRAME: u32 = 2;
pub const ENCODER_CONTROL_RECOVERY: u32 = 3;
pub const ENCODER_CONTROL_EFFECTIVE_STATE: u32 = 4;
pub const ENCODER_CONTROL_POLL: u32 = 5;
pub const ENCODER_CONTROL_FRAME_OPTIONS: u32 = 6;
pub const ENCODER_OUTCOME_ACCEPTED: u32 = 1;
pub const ENCODER_OUTCOME_PENDING: u32 = 2;
pub const ENCODER_OUTCOME_APPLIED: u32 = 3;
pub const ENCODER_OUTCOME_CLAMPED: u32 = 4;
pub const ENCODER_OUTCOME_UNSUPPORTED: u32 = 5;
pub const ENCODER_OUTCOME_RECREATE: u32 = 6;
pub const ENCODER_OUTCOME_FAILED: u32 = 7;
pub const ENCODER_OUTCOME_UNKNOWN: u32 = 8;
pub const ENCODER_EVENT_OUTPUT_READY: u32 = 1;
pub const ENCODER_EVENT_STATE_CHANGED: u32 = 2;
pub const ENCODER_EVENT_STOPPED: u32 = 3;

/// Namespace, value and schema are negotiated identifiers, not closed enums.
/// Unknown values require negotiation; they are not intrinsically invalid.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderFormatId {
    pub namespace_id: u64,
    pub value: u64,
    pub schema: u64,
}

/// Standard common configuration when options_schema is {standard, common
/// config, v1}. Native ABI struct, not a network encoding. size/version gate
/// the prefix. Geometry is pixels: visible rect (crop_x/y and visible width/
/// height) lies within the coded dimensions. fps is a nonzero rational; bitrate
/// is bits per second, with zero meaning unknown. Chroma and depth are negotiated IDs/values, zero means
/// unknown, never an implicit default. Profile/level are codec-specific: their
/// presence bits distinguish absent from a specified zero. Color is absent when
/// its presence bit is clear; present color uses FrameColorMetadata's version,
/// size and individual zero/unknown semantics. Unknown required values may be
/// rejected as unsupported. Reserved fields and unknown flags must be zero.
/// Output timestamps use output_clock: v1 STREAM_RELATIVE with nonzero epoch ID.
/// output_clock_origin_us is the agreed host-monotonic timestamp at that epoch;
/// checked addition maps relative timestamps to host monotonic microseconds.
/// Reject unsupported or unestablished clocks, never infer an origin.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderCommonConfigV1 {
    pub version: u32,
    pub size: u32,
    pub flags: u32,
    pub reserved: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub crop_x: u32,
    pub crop_y: u32,
    pub visible_width: u32,
    pub visible_height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub bitrate_bps: u64,
    pub profile: u32,
    pub level: u32,
    pub chroma: u32,
    pub bit_depth: u32,
    pub color: FrameColorMetadata,
    pub output_clock: FrameClockMetadata,
    pub output_clock_origin_us: u64,
}

/// Probe response schema {standard, provider limits, v1}. Limits are per
/// session and must not exceed the corresponding provider-wide constants.
/// Bytes include retained request, response and tombstone storage. Output bytes
/// bound each receive call. The descriptor is borrowed during the callback.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderProviderLimitsV1 {
    pub version: u32,
    pub size: u32,
    pub max_control_records: u32,
    pub reserved: u32,
    pub max_control_bytes: u64,
    pub max_output_bytes: u64,
}

/// Typed FRAME_OPTIONS request data for schema {standard, frame options, v1}.
/// Nested side_data is a synchronous borrow, at most MAX_OPAQUE_BYTES; its
/// namespace/schema is independently negotiated. Zero length denotes absent.
/// The request data length covers the whole struct, never its nested bytes.
/// Any bytes after the v1 prefix must be self-contained and pointer-free; their
/// exact contents are part of request identity. Providers must reject unsupported
/// required semantics explicitly, not interpret or dereference unknown pointers.
/// Pointer-bearing extensions require a new independently negotiated schema
/// version with defined ownership and copying rules.
/// Submission IDs are nonzero and never reused within a session; they are not
/// globally unique. source_id/source_generation identify the source epoch;
/// timestamp_us and clock match the corresponding input frame metadata.
///
/// Prepare via control and wait for APPLIED before submit. The prepared ID must
/// equal CaptureResourceFrame.sequence or both pool submission sequence and
/// control_id. A successful submit consumes metadata exactly once. A rejected
/// submit consumes nothing: retry only the same frame and metadata, or cancel.
/// A conflicting duplicate, foreign session, stale source/generation, or reused
/// ID is rejected. Cancel, session close and source/config generation change
/// discard unconsumed preparations; caller must prepare a new ID. Pending
/// preparation does not authorize submit. Provider copies any retained bytes;
/// per-session prepared count and bytes are bounded and excess returns BUSY.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderFrameOptionsV1 {
    pub version: u32,
    pub size: u32,
    pub submission_id: u64,
    pub source_id: u64,
    pub source_generation: u64,
    pub timestamp_us: u64,
    pub clock: FrameClockMetadata,
    pub side_data_schema: EncoderFormatId,
    pub side_data: BorrowedBytes,
}

/// All bytes are borrowed synchronously. Length is at most MAX_OPAQUE_BYTES;
/// null is valid only for zero length. Codec/framing and opaque schemas must be
/// mutually negotiated before create. Source mode needs no frame submissions.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderSessionConfig {
    pub size: u32,
    pub mode: u32,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub source_id: u64,
    pub config_generation: u64,
    pub options_schema: EncoderFormatId,
    pub options: BorrowedBytes,
}

/// Data is borrowed for the sink call only. Chunks with the same access_unit_id
/// are delivered in order and have matching identity/timing/format fields.
/// START and END mark boundaries; KEYFRAME and other picture flags apply to the
/// whole access unit. EOS has zero bytes and no picture boundary flags.
/// submission_id is the input frame sequence for submitted mode, or zero when
/// no submission corresponds to output. Timestamp is microseconds in the
/// configured output clock; source_id and config_generation identify epochs.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderOutputChunk {
    pub size: u32,
    pub flags: u32,
    pub session_id: u64,
    pub access_unit_id: u64,
    pub submission_id: u64,
    pub source_id: u64,
    pub config_generation: u64,
    pub timestamp_us: u64,
    pub codec: EncoderFormatId,
    pub framing: EncoderFormatId,
    pub bytes: BorrowedBytes,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderOutputSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const EncoderOutputChunk) -> Status,
}

/// Request IDs are caller-chosen, nonzero and unique within the session. Schema
/// and data define the negotiated operation payload (including recovery ranges).
/// Input bytes are borrowed for the call and limited to MAX_OPAQUE_BYTES.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderControlRequest {
    pub size: u32,
    pub kind: u32,
    pub request_id: u64,
    pub expected_generation: u64,
    pub schema: EncoderFormatId,
    pub data: BorrowedBytes,
}

/// A response is borrowed for the callback only. It echoes request_id and
/// reports the effective generation and negotiated state schema/data. A
/// successful reconfiguration takes effect at an indicated output generation;
/// older queued output retains its original generation. A response is not a
/// promise that a recovery/keyframe has already been emitted.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderControlResponse {
    pub size: u32,
    pub kind: u32,
    pub request_id: u64,
    pub effective_generation: u64,
    pub effective_submission_id: u64,
    pub outcome: u32,
    pub reserved: u32,
    pub schema: EncoderFormatId,
    pub data: BorrowedBytes,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderControlSink {
    pub context: *mut c_void,
    pub write: unsafe extern "C" fn(*mut c_void, *const EncoderControlResponse) -> Status,
}

/// Notifications may coalesce and run concurrently on provider threads. A
/// callback only records the event and wakes the consumer; it must not reenter
/// the provider. No borrowed event data is retained. Subscribe announces already
/// available output or stopped state, closing the registration race.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EncoderEventSink {
    pub context: *mut c_void,
    pub notify: unsafe extern "C" fn(*mut c_void, u64, u32),
}

/// Optional separately registered native service. The authorized control service
/// creates, looks up and closes sessions with principal-scoped idempotent operation
/// IDs; this table never creates an authorized session. configure_session binds an
/// already authorized session to negotiated config and signed policy. Provider,
/// resource consumer and input pool are distinct signed services and grants;
/// they must belong to the same provider and authenticated owner, share its
/// session-ID namespace and lifetime, and reject foreign or closed sessions.
/// The host selects this coherent service set; a session ID is also the stream ID
/// of the associated FrameResourceConsumerApi. Source mode requires no submits.
/// Existing consumer submit and synchronous cancel semantics remain unchanged.
///
/// probe is side-effect-free and evaluates a candidate before configure. Its
/// bounded response schema/data describe supported limits and effective format;
/// unsupported and unknown outcomes do not invalidate unknown negotiated IDs.
/// Per-session operations are caller-serialized except notifications. receive
/// invokes at most one synchronous sink; OK consumes the chunk, rejection
/// returns its status and retains the identical chunk. A smaller retry bound
/// returns BUSY without callback; PENDING invokes none. max_bytes is 1 through
/// MAX_OUTPUT_BYTES. Buffers/callbacks never outlive their documented call.
/// No unwind crosses the ABI.
///
/// Control request IDs are deduplicated per session. Retrying the same ID and
/// payload retrieves its stored result; a different payload with that ID is
/// rejected. POLL uses the original ID and empty data. ACCEPTED/PENDING are not
/// effective state; APPLIED/CLAMPED report actual generation and submission
/// boundary. UNSUPPORTED/RECREATE/FAILED/UNKNOWN are distinct outcomes. Sink
/// rejection retains the exact response for retry and never undoes hardware
/// effects. Request/result and tombstone storage survives until session close.
/// A provider advertises limits no greater than MAX_CONTROL_RECORDS and
/// MAX_CONTROL_BYTES, including all request, result and tombstone storage. Before
/// any side effect for a new ID it reserves enough for its complete result and
/// replay record; if either bound would be exceeded it returns BUSY without
/// admitting the ID. Once accepted, same-request retries and POLL are admitted
/// even at capacity. No eviction can lose exact replay or permit ID reuse; once
/// full, new IDs may stay BUSY until close. drain stops
/// admission, completes accepted work and emits exactly one EOS; repeated drain
/// is idempotent. destroy_session synchronously unsubscribes, cancels resource
/// submissions and retires work; the control service then closes the session.
/// If an input pool is in use, its retirement must first report RETIRED; while
/// pending or quarantined, defer destroy, close and binding release and keep all
/// required resources live until actual proof of retirement or process exit.
/// Keep all service bindings alive through cancellation and destruction. Release
/// must perform these barriers for its sessions. No asynchronous retirement is
/// implied.
#[repr(C)]
pub struct EncoderProviderApi {
    pub base: NativeInterface,
    pub probe: unsafe extern "C" fn(
        *mut c_void,
        *const EncoderSessionConfig,
        EncoderControlSink,
    ) -> Status,
    pub configure_session:
        unsafe extern "C" fn(*mut c_void, u64, *const EncoderSessionConfig) -> Status,
    pub receive: unsafe extern "C" fn(*mut c_void, u64, usize, EncoderOutputSink) -> Status,
    pub control: unsafe extern "C" fn(
        *mut c_void,
        u64,
        *const EncoderControlRequest,
        EncoderControlSink,
    ) -> Status,
    /// Both event functions are present together or both absent. At most one
    /// subscription per session. unsubscribe is an idempotent synchronous
    /// barrier for all admitted callbacks; never call it from a callback.
    pub subscribe: Option<unsafe extern "C" fn(*mut c_void, u64, EncoderEventSink) -> Status>,
    pub unsubscribe: Option<unsafe extern "C" fn(*mut c_void, u64)>,
    pub drain: unsafe extern "C" fn(*mut c_void, u64) -> Status,
    pub destroy_session: unsafe extern "C" fn(*mut c_void, u64),
}

pub const ENCODER_PROVIDER_API_V1_0_SIZE: u32 = size_of::<EncoderProviderApi>() as u32;

pub fn encoder_provider_available(header: AbiHeader) -> bool {
    header.major == ENCODER_PROVIDER_INTERFACE_MAJOR
        && header.size >= ENCODER_PROVIDER_API_V1_0_SIZE
}

/// Checks admission against already-accounted per-session record and byte use.
/// Reserve complete request, result and tombstone storage before side effects.
/// The caller must serialize admission and account actual retained allocations.
pub fn encoder_control_capacity_available(
    record_count: usize,
    used_bytes: usize,
    new_record_bytes: usize,
    record_limit: usize,
    byte_limit: usize,
) -> bool {
    record_limit <= ENCODER_PROVIDER_MAX_CONTROL_RECORDS
        && byte_limit <= ENCODER_PROVIDER_MAX_CONTROL_BYTES
        && record_count < record_limit
        && used_bytes
            .checked_add(new_record_bytes)
            .is_some_and(|sum| sum <= byte_limit)
}

/// Validates geometry and reserved bits, without judging negotiated IDs.
pub fn encoder_common_config_valid(config: &EncoderCommonConfigV1) -> bool {
    let flags = ENCODER_CONFIG_HAS_PROFILE | ENCODER_CONFIG_HAS_LEVEL | ENCODER_CONFIG_HAS_COLOR;
    config.version == 1
        && config.size >= size_of::<EncoderCommonConfigV1>() as u32
        && config.flags & !flags == 0
        && config.reserved == 0
        && config.coded_width != 0
        && config.coded_height != 0
        && config.visible_width != 0
        && config.visible_height != 0
        && config
            .crop_x
            .checked_add(config.visible_width)
            .is_some_and(|x| x <= config.coded_width)
        && config
            .crop_y
            .checked_add(config.visible_height)
            .is_some_and(|y| y <= config.coded_height)
        && config.fps_numerator != 0
        && config.fps_denominator != 0
        && config.output_clock.version == 1
        && config.output_clock.size >= size_of::<FrameClockMetadata>() as u32
        && config.output_clock.domain == FRAME_CLOCK_STREAM_RELATIVE
        && config.output_clock.reserved == 0
        && config.output_clock.epoch_id != [0; 16]
}

/// Checks the envelope without dereferencing or interpreting side data.
pub fn encoder_frame_options_valid(options: &EncoderFrameOptionsV1) -> bool {
    options.version == 1
        && options.size >= size_of::<EncoderFrameOptionsV1>() as u32
        && options.submission_id != 0
        && encoder_borrowed_bytes_valid(options.side_data, ENCODER_PROVIDER_MAX_OPAQUE_BYTES)
}

/// Checks the known v1 prefix and input shape, without dereferencing pointers.
/// Negotiation and readable pointer validation remain the caller's responsibility.
pub fn encoder_session_config_valid(config: &EncoderSessionConfig) -> bool {
    config.size >= size_of::<EncoderSessionConfig>() as u32
        && matches!(
            config.mode,
            ENCODER_SESSION_SUBMITTED | ENCODER_SESSION_SOURCE
        )
        && encoder_borrowed_bytes_valid(config.options, ENCODER_PROVIDER_MAX_OPAQUE_BYTES)
}

pub fn encoder_control_request_valid(request: &EncoderControlRequest) -> bool {
    request.size >= size_of::<EncoderControlRequest>() as u32
        && request.request_id != 0
        && encoder_borrowed_bytes_valid(request.data, ENCODER_PROVIDER_MAX_OPAQUE_BYTES)
}

/// Checks descriptor shape only; callers must still negotiate IDs and validate
/// pointer readability at their ABI boundary before accessing bytes.
pub fn encoder_borrowed_bytes_valid(bytes: BorrowedBytes, max: usize) -> bool {
    bytes.len <= max && (bytes.len == 0 || !bytes.ptr.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_capacity_enforces_negotiated_bounds_and_overflow() {
        assert!(encoder_control_capacity_available(1, 100, 200, 2, 300));
        assert!(!encoder_control_capacity_available(2, 100, 200, 2, 300));
        assert!(!encoder_control_capacity_available(1, 101, 200, 2, 300));
        assert!(!encoder_control_capacity_available(
            1,
            usize::MAX,
            1,
            2,
            300
        ));
        assert!(!encoder_control_capacity_available(
            0,
            0,
            1,
            ENCODER_PROVIDER_MAX_CONTROL_RECORDS + 1,
            300
        ));
        assert!(!encoder_control_capacity_available(
            0,
            0,
            1,
            1,
            ENCODER_PROVIDER_MAX_CONTROL_BYTES + 1
        ));
    }
    #[test]
    fn checks_version_size_and_byte_shapes() {
        let mut header = AbiHeader {
            size: ENCODER_PROVIDER_API_V1_0_SIZE,
            major: 1,
            minor: 0,
        };
        assert!(encoder_provider_available(header));
        header.size -= 1;
        assert!(!encoder_provider_available(header));
        header.size += 1;
        header.major = 2;
        assert!(!encoder_provider_available(header));
        assert!(encoder_borrowed_bytes_valid(
            BorrowedBytes {
                ptr: std::ptr::null(),
                len: 0
            },
            3
        ));
        assert!(!encoder_borrowed_bytes_valid(
            BorrowedBytes {
                ptr: std::ptr::null(),
                len: 1
            },
            3
        ));
        assert!(!encoder_borrowed_bytes_valid(
            BorrowedBytes {
                ptr: std::ptr::NonNull::<u8>::dangling().as_ptr(),
                len: 4
            },
            3
        ));
    }
    #[test]
    fn config_and_control_validate_prefix_without_rejecting_unknown_ids() {
        let id = EncoderFormatId {
            namespace_id: 99,
            value: 999,
            schema: 7,
        };
        let empty = BorrowedBytes {
            ptr: std::ptr::null(),
            len: 0,
        };
        let mut config = EncoderSessionConfig {
            size: size_of::<EncoderSessionConfig>() as u32,
            mode: ENCODER_SESSION_SOURCE,
            codec: id,
            framing: id,
            source_id: 9,
            config_generation: 1,
            options_schema: id,
            options: empty,
        };
        assert!(encoder_session_config_valid(&config));
        config.size -= 1;
        assert!(!encoder_session_config_valid(&config));
        config.size += 1;
        config.mode = 0;
        assert!(!encoder_session_config_valid(&config));
        let mut request = EncoderControlRequest {
            size: size_of::<EncoderControlRequest>() as u32,
            kind: 999,
            request_id: 1,
            expected_generation: 1,
            schema: id,
            data: empty,
        };
        assert!(encoder_control_request_valid(&request));
        request.request_id = 0;
        assert!(!encoder_control_request_valid(&request));
    }

    #[test]
    fn common_config_validates_crop_overflow_and_frame_ids() {
        let mut config = EncoderCommonConfigV1 {
            version: 1,
            size: size_of::<EncoderCommonConfigV1>() as u32,
            flags: 0,
            reserved: 0,
            coded_width: 1920,
            coded_height: 1080,
            crop_x: 0,
            crop_y: 0,
            visible_width: 1920,
            visible_height: 1080,
            fps_numerator: 60,
            fps_denominator: 1,
            bitrate_bps: 0,
            profile: 0,
            level: 0,
            chroma: 999,
            bit_depth: 0,
            color: FrameColorMetadata::default(),
            output_clock: FrameClockMetadata {
                version: 1,
                size: size_of::<FrameClockMetadata>() as u32,
                domain: FRAME_CLOCK_STREAM_RELATIVE,
                reserved: 0,
                epoch_id: [1; 16],
            },
            output_clock_origin_us: 10,
        };
        assert!(encoder_common_config_valid(&config));
        config.output_clock.epoch_id = [0; 16];
        assert!(!encoder_common_config_valid(&config));
        config.output_clock.epoch_id = [1; 16];
        config.crop_x = u32::MAX;
        assert!(!encoder_common_config_valid(&config));
        let mut options = EncoderFrameOptionsV1 {
            version: 1,
            size: size_of::<EncoderFrameOptionsV1>() as u32,
            submission_id: 1,
            source_id: 1,
            source_generation: 1,
            timestamp_us: 0,
            clock: FrameClockMetadata::default(),
            side_data_schema: EncoderFormatId {
                namespace_id: 9,
                value: 9,
                schema: 9,
            },
            side_data: BorrowedBytes {
                ptr: std::ptr::null(),
                len: 0,
            },
        };
        assert!(encoder_frame_options_valid(&options));
        options.submission_id = 0;
        assert!(!encoder_frame_options_valid(&options));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layout_matches_c() {
        assert_eq!(size_of::<EncoderFormatId>(), 24);
        assert_eq!(size_of::<EncoderCommonConfigV1>(), 136);
        assert_eq!(size_of::<EncoderFrameOptionsV1>(), 112);
        assert_eq!(size_of::<EncoderProviderLimitsV1>(), 32);
        assert_eq!(size_of::<EncoderSessionConfig>(), 112);
        assert_eq!(size_of::<EncoderOutputChunk>(), 120);
        assert_eq!(size_of::<EncoderControlRequest>(), 64);
        assert_eq!(size_of::<EncoderControlResponse>(), 80);
        assert_eq!(size_of::<EncoderProviderApi>(), 96);
        assert_eq!(std::mem::offset_of!(EncoderProviderApi, probe), 32);
    }
}
