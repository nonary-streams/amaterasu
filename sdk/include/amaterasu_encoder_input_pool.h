#ifndef AMATERASU_ENCODER_INPUT_POOL_H
#define AMATERASU_ENCODER_INPUT_POOL_H
#include "amaterasu_media.h"
#ifdef __cplusplus
extern "C" {
#endif
/* Pool-only writable CPU schema: FrameDescriptor kind=3, version=1,
 * zero namespace. Offer data is capabilities; acquired data is writable frame.
 * BGRA8 and NV12 only; NONE acquire/completion sync; CPU writes must complete
 * before commit or abandon. Alignment values are nonzero powers of two. */
#define AMATERASU_ENCODER_INPUT_RESOURCE_CPU_WRITABLE 3u
typedef struct {
    uint32_t format, min_width, max_width, min_height, max_height;
    uint32_t stride_alignment, data_alignment;
} AmaterasuEncoderInputCpuWritableCapabilities;
/* data is provider-owned, exclusively borrowed mutable storage for this lease;
 * len is capacity, at least the complete padded image size. Validate dimensions,
 * format, overflow, capacity and alignment. Source and target must not overlap.
 * Access only after acquire and before successful commit/abandon; do not retain
 * or free the pointer. No other reference may access the mutable allocation. */
typedef struct {
    uint32_t width, height, format, stride;
    uint8_t *data;
    size_t len;
} AmaterasuEncoderInputCpuWritableFrame;
#define AMATERASU_ENCODER_INPUT_POOL_INTERFACE_MAJOR 1u
#define AMATERASU_ENCODER_INPUT_POOL_INTERFACE_MINOR 0u
#define AMATERASU_ENCODER_INPUT_POOL_MAX_TARGETS 64u
#define AMATERASU_ENCODER_INPUT_POOL_RETIRED 1u
#define AMATERASU_ENCODER_INPUT_POOL_QUARANTINED 2u

typedef AmaterasuCaptureResourceOffer AmaterasuEncoderInputPoolOffer;
typedef AmaterasuCaptureResourceOfferSink AmaterasuEncoderInputPoolOfferSink;

/* v1 size must be at least sizeof; IDs, dimensions and count are nonzero,
 * count <= MAX_TARGETS, and reserved is zero. Configure validates the actual
 * schemas, device and dimensions and changes nothing on failure. Success creates
 * a fresh nonzero pool generation, including for an unchanged size. */
typedef struct {
    uint32_t version, size;
    uint64_t offer_generation, offer_id;
    uint32_t width, height, target_count, reserved;
} AmaterasuEncoderInputPoolConfig;

/* All IDs are nonzero. target_id is unique in a generation and lease_id is
 * never reused within a stream. Resource and sync descriptors and handles are
 * borrowed until this lease is consumed. Wait on acquire_sync and any resource
 * plane fences before writing. NONE asserts all preceding encoder reads ended. */
typedef struct {
    uint64_t generation, target_id, lease_id;
    AmaterasuFrameDescriptor resource, acquire_sync;
} AmaterasuEncoderInputTarget;

/* When frame options are prepared, control_id and sequence both equal the
 * nonzero EncoderFrameOptionsV1 submission_id, and session, source epoch,
 * timestamp and clock match the preparation. Otherwise control_id is zero.
 * Rejected commit preserves preparation for retry of the same frame and metadata
 * or cancellation. completion_sync covers every writer access; NONE asserts
 * completion. The encoder duplicates needed fence data and handles before
 * returning success and waits before reading. */
typedef struct {
    uint32_t version, size;
    uint64_t generation, target_id, lease_id, control_id, timestamp_us, sequence;
    AmaterasuFrameColorMetadata color;
    AmaterasuFrameClockMetadata clock;
    AmaterasuFrameDescriptor completion_sync;
} AmaterasuEncoderInputSubmission;

/* RETIRED confirms completion of all accesses. QUARANTINED means completion
 * is uncertain and storage must remain isolated, not reused. */
typedef struct {
    uint32_t version, size;
    uint64_t generation;
    uint32_t state, reserved;
} AmaterasuEncoderInputPoolRetirement;

/* Separately bound signed native service vendor.encode-pool/1, linked to the
 * owning encoder provider's authenticated principal and session namespace.
 * Native queries cannot distinguish separate major-1 tables by type. Pool and
 * encoder bindings share session identity and stay alive as resources require.
 * Offers use resource schemas: resource is writable target capability,
 * acquire_sync is encoder-to-writer, completion_sync is writer-to-encoder.
 * Check exact kind, version, namespace, device, format and sync compatibility;
 * unknown device is not a wildcard. Enumeration and sink calls are synchronous;
 * empty enumeration still has nonzero generation. Offers are borrowed only
 * through the callback.
 *
 * Acquire grants one exclusive lease or BUSY without a lease. Commit success
 * consumes the lease once and queues encoding. Failure schedules no read and
 * leaves ownership with the writer. Abandon consumes the lease only on success
 * and supplies completion for all writer accesses, including pending GPU work.
 * Reject stale, duplicate, wrong-stream and consumed identities. Never reuse
 * storage before all writer and encoder accesses complete; invalid or failed
 * fences require quarantine.
 *
 * Reconfigure stops old acquisitions; old leases can be abandoned, but stale
 * commits fail without consuming them. Successful begin_retirement stops new
 * acquire, configure and commit admission. Existing leases must still be
 * abandoned with completion; an error leaves admission unchanged. No concurrent
 * calls are permitted. Retirement returns PENDING while work or leases remain,
 * RETIRED only when all accesses are proven complete, or QUARANTINED when
 * completion is uncertain. Until RETIRED, pool and encoder bindings, code and
 * all source, device and storage resources remain live until independently
 * proven safe. Process exit ends in-process ownership, but does not prove
 * external GPU, DMA or shared-handle access stopped. Safe termination requires
 * proven completion or platform/driver cleanup guarantees. Timeout or unknown
 * GPU state is not proof. Quarantine may become RETIRED only with actual proof.
 * This protocol does not
 * alter existing synchronous cancel contracts. Callbacks must not unwind. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*enumerate)(void *, uint64_t, AmaterasuEncoderInputPoolOfferSink, uint64_t *);
    AmaterasuStatus (*configure)(void *, uint64_t, const AmaterasuEncoderInputPoolConfig *, uint64_t *);
    AmaterasuStatus (*acquire)(void *, uint64_t, uint64_t, AmaterasuEncoderInputTarget *);
    AmaterasuStatus (*commit)(void *, uint64_t, const AmaterasuEncoderInputSubmission *);
    AmaterasuStatus (*abandon)(void *, uint64_t, uint64_t, uint64_t, uint64_t, const AmaterasuFrameDescriptor *);
    AmaterasuStatus (*begin_retirement)(void *, uint64_t);
    AmaterasuStatus (*retirement)(void *, uint64_t, uint64_t, AmaterasuEncoderInputPoolRetirement *);
} AmaterasuEncoderInputPoolApiV1;
#define AMATERASU_ENCODER_INPUT_POOL_API_V1_SIZE ((uint32_t)sizeof(AmaterasuEncoderInputPoolApiV1))
#ifdef __cplusplus
}
#endif
#endif
