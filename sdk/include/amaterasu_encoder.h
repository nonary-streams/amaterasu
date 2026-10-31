#ifndef AMATERASU_ENCODER_H
#define AMATERASU_ENCODER_H
#include "amaterasu_media.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Optional separate interface; the existing EncoderApi v1.x is unchanged. */
#define AMATERASU_ENCODER_PROVIDER_INTERFACE_MAJOR 1u
#define AMATERASU_ENCODER_PROVIDER_INTERFACE_MINOR 0u
#define AMATERASU_ENCODER_PROVIDER_MAX_OPAQUE_BYTES (64u * 1024u)
#define AMATERASU_ENCODER_PROVIDER_MAX_OUTPUT_BYTES (16u * 1024u * 1024u)
#define AMATERASU_ENCODER_PROVIDER_MAX_CONTROL_RECORDS 1024u
#define AMATERASU_ENCODER_PROVIDER_MAX_CONTROL_BYTES (8u * 1024u * 1024u)
/* Standard namespace/schema 1. Codec values use AMATERASU_CODEC_H264,
 * AMATERASU_CODEC_HEVC and AMATERASU_CODEC_AV1. Length-prefix width and other
 * framing details must be negotiated in configuration. */
#define AMATERASU_ENCODER_FORMAT_NAMESPACE_STANDARD 1u
#define AMATERASU_ENCODER_FORMAT_SCHEMA_V1 1u
#define AMATERASU_ENCODER_FRAMING_ANNEX_B 1u
#define AMATERASU_ENCODER_FRAMING_LENGTH_PREFIXED 2u
#define AMATERASU_ENCODER_FRAMING_AV1_OBU 3u
#define AMATERASU_ENCODER_SCHEMA_COMMON_CONFIG 1u
#define AMATERASU_ENCODER_SCHEMA_FRAME_OPTIONS 2u
#define AMATERASU_ENCODER_SCHEMA_PROVIDER_LIMITS 3u
#define AMATERASU_ENCODER_CONFIG_HAS_PROFILE 1u
#define AMATERASU_ENCODER_CONFIG_HAS_LEVEL 2u
#define AMATERASU_ENCODER_CONFIG_HAS_COLOR 4u
#define AMATERASU_ENCODER_CHROMA_420 1u
#define AMATERASU_ENCODER_CHROMA_422 2u
#define AMATERASU_ENCODER_CHROMA_444 3u
#define AMATERASU_ENCODER_CHROMA_MONOCHROME 4u
#define AMATERASU_ENCODER_SESSION_SUBMITTED 1u
#define AMATERASU_ENCODER_SESSION_SOURCE 2u
#define AMATERASU_ENCODER_CHUNK_ACCESS_UNIT_START 1u
#define AMATERASU_ENCODER_CHUNK_ACCESS_UNIT_END 2u
#define AMATERASU_ENCODER_CHUNK_KEYFRAME 4u
#define AMATERASU_ENCODER_CHUNK_DISCONTINUITY 8u
#define AMATERASU_ENCODER_CHUNK_CORRUPT 16u
#define AMATERASU_ENCODER_CHUNK_END_OF_STREAM 32u
#define AMATERASU_ENCODER_CONTROL_RECONFIGURE 1u
#define AMATERASU_ENCODER_CONTROL_REQUEST_KEYFRAME 2u
#define AMATERASU_ENCODER_CONTROL_RECOVERY 3u
#define AMATERASU_ENCODER_CONTROL_EFFECTIVE_STATE 4u
#define AMATERASU_ENCODER_CONTROL_POLL 5u
#define AMATERASU_ENCODER_CONTROL_FRAME_OPTIONS 6u
#define AMATERASU_ENCODER_OUTCOME_ACCEPTED 1u
#define AMATERASU_ENCODER_OUTCOME_PENDING 2u
#define AMATERASU_ENCODER_OUTCOME_APPLIED 3u
#define AMATERASU_ENCODER_OUTCOME_CLAMPED 4u
#define AMATERASU_ENCODER_OUTCOME_UNSUPPORTED 5u
#define AMATERASU_ENCODER_OUTCOME_RECREATE 6u
#define AMATERASU_ENCODER_OUTCOME_FAILED 7u
#define AMATERASU_ENCODER_OUTCOME_UNKNOWN 8u
#define AMATERASU_ENCODER_EVENT_OUTPUT_READY 1u
#define AMATERASU_ENCODER_EVENT_STATE_CHANGED 2u
#define AMATERASU_ENCODER_EVENT_STOPPED 3u

/* IDs are negotiated, not closed enums. Unknown IDs require negotiation. */
typedef struct { uint64_t namespace_id, value, schema; } AmaterasuEncoderFormatId;
/* options_schema {standard, common config, v1}: native ABI struct, not a
 * network encoding. Visible rect is (crop_x, crop_y, visible_width,
 * visible_height) and must fit coded size. FPS is a nonzero rational; bitrate
 * is bits per second (zero means unknown). Chroma and bit depth zero mean unknown, not default.
 * Profile and level are codec-specific and presence flags distinguish absent
 * from specified zero. Color is absent without HAS_COLOR; otherwise its own
 * version, size and zero/unknown fields apply. Unknown required values may be
 * unsupported. Reserved and unknown flag bits must be zero. output_clock is v1
 * STREAM_RELATIVE with a nonzero epoch_id; output_clock_origin_us is the agreed
 * host-monotonic microseconds at that epoch. Checked addition maps relative
 * timestamps to host monotonic time. Reject unestablished/unsupported clocks.  */
typedef struct {
    uint32_t version, size, flags, reserved;
    uint32_t coded_width, coded_height, crop_x, crop_y, visible_width, visible_height;
    uint32_t fps_numerator, fps_denominator;
    uint64_t bitrate_bps;
    uint32_t profile, level, chroma, bit_depth;
    AmaterasuFrameColorMetadata color;
    AmaterasuFrameClockMetadata output_clock;
    uint64_t output_clock_origin_us;
} AmaterasuEncoderCommonConfigV1;
/* Probe response schema {standard, provider limits, v1}, borrowed in callback.
 * Per-session limits cannot exceed global constants; bytes include requests,
 * results and tombstones. max_output_bytes bounds a receive call. */
typedef struct {
    uint32_t version, size, max_control_records, reserved;
    uint64_t max_control_bytes, max_output_bytes;
} AmaterasuEncoderProviderLimitsV1;
/* FRAME_OPTIONS request data with schema {standard, frame options, v1}. The
 * struct is borrowed during control; its separately negotiated side_data is
 * borrowed then too, bounded by MAX_OPAQUE_BYTES, zero length means absent.
 * Request data length covers the struct, not nested data. Appended v1 bytes must
 * be self-contained and pointer-free and are part of request identity. Reject
 * unsupported required semantics explicitly; never interpret or dereference
 * unknown pointers. Pointer-bearing extensions require a new negotiated schema
 * version with defined ownership and copy rules. Nonzero submission
 * IDs are never reused within a session and are scoped to it. source_id and
 * source_generation identify the source epoch. Timestamp/clock match input.
 *
 * Prepare with control and wait for APPLIED before submit. ID equals frame
 * sequence, or both pool sequence and control_id. Success consumes preparation
 * once. Rejected submit consumes nothing: retry only that frame and metadata,
 * or cancel. Conflicting duplicate, foreign session, stale source/generation,
 * or reused ID is rejected. Cancel, close, source/config generation change
 * discard unconsumed preparations; prepare a new ID. Pending does not authorize
 * submit. Provider copies retained bytes; bounded preparation storage returns
 * BUSY when full. */
typedef struct {
    uint32_t version, size;
    uint64_t submission_id, source_id, source_generation, timestamp_us;
    AmaterasuFrameClockMetadata clock;
    AmaterasuEncoderFormatId side_data_schema;
    AmaterasuBorrowedBytes side_data;
} AmaterasuEncoderFrameOptionsV1;
/* Byte fields are borrowed only for the call; each is bounded by
 * MAX_OPAQUE_BYTES and null only for zero length. Codec, framing and option
 * schemas must be mutually negotiated before create. */
typedef struct {
    uint32_t size, mode;
    AmaterasuEncoderFormatId codec, framing;
    uint64_t source_id, config_generation;
    AmaterasuEncoderFormatId options_schema;
    AmaterasuBorrowedBytes options;
} AmaterasuEncoderSessionConfig;
/* Borrowed only during write. Chunks of an access unit have the same identity,
 * timing and formats, arrive in order, and START/END mark its boundaries.
 * Picture flags apply to the entire unit. EOS has zero bytes and no picture
 * boundary flags. submission_id is the input frame sequence for submitted mode,
 * or zero when no submission corresponds. Timestamp is microseconds in the
 * configured output clock. source_id and config_generation identify epochs. */
typedef struct {
    uint32_t size, flags;
    uint64_t session_id, access_unit_id, submission_id, source_id, config_generation, timestamp_us;
    AmaterasuEncoderFormatId codec, framing;
    AmaterasuBorrowedBytes bytes;
} AmaterasuEncoderOutputChunk;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuEncoderOutputChunk *);
} AmaterasuEncoderOutputSink;
/* Nonzero request IDs are unique per session. Data is borrowed during the call,
 * at most MAX_OPAQUE_BYTES, and has a mutually negotiated schema. Recovery
 * ranges and vendor operations are expressed in negotiated payloads. */
typedef struct {
    uint32_t size, kind;
    uint64_t request_id, expected_generation;
    AmaterasuEncoderFormatId schema;
    AmaterasuBorrowedBytes data;
} AmaterasuEncoderControlRequest;
/* Borrowed for callback only; data is bounded as above. Echoes request ID and
 * effective generation. Older queued output retains its original generation.
 * A response does not promise that a recovery/keyframe was already emitted. */
typedef struct {
    uint32_t size, kind;
    uint64_t request_id, effective_generation, effective_submission_id;
    uint32_t outcome, reserved;
    AmaterasuEncoderFormatId schema;
    AmaterasuBorrowedBytes data;
} AmaterasuEncoderControlResponse;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuEncoderControlResponse *);
} AmaterasuEncoderControlSink;
/* Notifications may coalesce and run concurrently on provider threads. The
 * callback only records and wakes; it must not reenter the provider. */
typedef struct {
    void *context;
    void (*notify)(void *, uint64_t session_id, uint32_t event);
} AmaterasuEncoderEventSink;
/* Independently registered native service; check major and size. The authorized
 * control service creates/looks up/closes sessions with principal-scoped,
 * idempotent operation IDs. configure_session binds an already authorized ID to
 * negotiated configuration and signed policy. Provider, resource consumer and
 * pool are distinct signed services/grants, selected as a coherent set by the
 * host: same provider and authenticated owner, shared session namespace and
 * lifetime; reject foreign/closed IDs. Session ID is the consumer stream ID.
 * Source sessions submit nothing. Existing synchronous cancel is unchanged.
 *
 * probe is side-effect-free and evaluates a candidate before configuration. Its
 * bounded response schema/data describe supported limits and effective format;
 * unknown IDs require negotiation. Session calls are serialized except event
 * callbacks; no unwind crosses the ABI. receive delivers at most one borrowed
 * chunk per call, bounded by max_bytes (1..MAX_OUTPUT_BYTES). Accepted chunks
 * are consumed; rejection preserves the identical chunk. A smaller retry bound
 * returns BUSY without callback; PENDING calls none.
 *
 * Control request IDs are deduplicated per session: same ID and payload retrieves
 * stored result; conflicting payload is rejected. POLL uses the original ID
 * and empty data. ACCEPTED/PENDING are not effective; APPLIED/CLAMPED report
 * effective generation and submission boundary. UNSUPPORTED/RECREATE/FAILED/
 * UNKNOWN are distinct outcomes. Sink rejection retains the exact response
 * and never rolls back hardware. Results and replay tombstones survive until
 * session close. A provider advertises lower or equal per-session limits for
 * record count and aggregate request/result/tombstone storage. Before any side
 * effects for a new ID, reserve its complete result and replay record; if either
 * limit would be exceeded, return BUSY without admitting the ID. Same-request
 * retries and POLL remain admitted even at capacity. No eviction may lose exact
 * replay or permit ID reuse; new IDs may remain BUSY until close when full.
 *
 * Event functions are both present or both null; one subscription per session.
 * Subscribe announces existing output/stopped state. Unsubscribe is an
 * idempotent barrier; never call it in a callback. Drain stops admission,
 * completes accepted work and emits exactly one EOS; it is idempotent. Destroy
 * synchronously unsubscribes, cancels submissions and retires work; control
 * service closes the session. If an input pool is used, wait for its RETIRED
 * result before destroy, close or binding release. Pending or quarantined pool
 * resources and bindings remain live until actual retirement proof or exit.
 * Keep service bindings live through these barriers.
 * Release does the same for sessions. No asynchronous retirement is implied. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*probe)(void *, const AmaterasuEncoderSessionConfig *, AmaterasuEncoderControlSink);
    AmaterasuStatus (*configure_session)(void *, uint64_t, const AmaterasuEncoderSessionConfig *);
    AmaterasuStatus (*receive)(void *, uint64_t, size_t, AmaterasuEncoderOutputSink);
    AmaterasuStatus (*control)(void *, uint64_t, const AmaterasuEncoderControlRequest *, AmaterasuEncoderControlSink);
    AmaterasuStatus (*subscribe)(void *, uint64_t, AmaterasuEncoderEventSink);
    void (*unsubscribe)(void *, uint64_t);
    AmaterasuStatus (*drain)(void *, uint64_t);
    void (*destroy_session)(void *, uint64_t);
} AmaterasuEncoderProviderApi;
#define AMATERASU_ENCODER_PROVIDER_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuEncoderProviderApi))
#ifdef __cplusplus
}
#endif
#endif
