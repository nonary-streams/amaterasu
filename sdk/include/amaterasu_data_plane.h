#ifndef AMATERASU_DATA_PLANE_H
#define AMATERASU_DATA_PLANE_H
#include "amaterasu.h"
#ifdef __cplusplus
extern "C" {
#endif

/* Protocol-neutral CPU binary transfer. See docs/binary-transfer-api.md for
 * authorization, concurrency, ordering, ownership and teardown contracts.
 * This is a native ABI, never a network wire encoding. */
/* Register and bind with this family through the typed native commands.
 * Untyped factories and other families are rejected for every service alias. */
#define AMATERASU_DATA_TRANSFER_CONTRACT "amaterasu.binary-transfer.native/1"
#define AMATERASU_DATA_INTERFACE_MAJOR 1u
#define AMATERASU_DATA_INTERFACE_MINOR 0u
#define AMATERASU_DATA_BUSY 30
#define AMATERASU_DATA_CLOSED 31
#define AMATERASU_DATA_UNSUPPORTED 32
#define AMATERASU_DATA_DENIED 33
#define AMATERASU_DATA_TOO_LARGE 34
#define AMATERASU_DATA_EMPTY 35
#define AMATERASU_DATA_TIMED_OUT 36
#define AMATERASU_DATA_MAX_SEGMENTS 256u
#define AMATERASU_DATA_MAX_METADATA_BYTES 65536u
#define AMATERASU_DATA_MAX_WAIT_MS 60000u
#define AMATERASU_DATA_SOURCE 1u
#define AMATERASU_DATA_SINK 2u
#define AMATERASU_DATA_ACTIVE 1u
#define AMATERASU_DATA_DRAINING 2u
#define AMATERASU_DATA_ENDED 3u
#define AMATERASU_DATA_CANCELLED 4u

typedef struct {
    uint64_t namespace_id;
    uint64_t value;
    uint64_t version;
} AmaterasuDataSchema;
typedef struct {
    uint64_t incarnation;
    uint64_t sequence;
} AmaterasuDataChannelId;
typedef struct {
    uint64_t max_chunk_bytes;
    uint64_t max_queued_bytes;
    uint64_t max_record_bytes;
    uint32_t max_queued_chunks;
    uint32_t max_segments;
    uint32_t max_metadata_bytes;
    uint32_t reserved;
} AmaterasuDataLimits;
typedef struct {
    uint32_t size;
    uint32_t version;
    uint32_t direction;
    uint32_t reserved;
    AmaterasuDataSchema format;
    AmaterasuDataSchema metadata_schema;
    AmaterasuDataLimits limits;
} AmaterasuDataDescription;
typedef struct {
    const uint8_t *data;
    size_t len;
} AmaterasuDataSegment;

/* Immutable retained CPU storage. Callbacks are thread-safe, nonblocking,
 * infallible, do not reenter a transfer channel and never unwind. Every retain
 * pairs with one release, in the PRODUCER's allocator/library. Pinned payload
 * backing storage including unused capacity is charged as retained_bytes.
 * Producer DLL must outlive cancel/release of all consumers. Not a GPU fence. */
typedef struct {
    void *context;
    uint64_t retained_bytes;
    void (*retain)(void *context);
    void (*release)(void *context);
} AmaterasuDataBufferLease;

/* Ordered fragments, not datagrams. New record IDs strictly increase; offsets
 * are contiguous, total_bytes constant. Metadata is pointer-free and occurs
 * only at offset 0. Pointer arrays and bytes are immutable synchronous borrows,
 * or covered by lease for submission. Receive ALWAYS has lease == NULL. */
typedef struct {
    uint32_t size;
    uint32_t reserved;
    uint64_t record_id;
    uint64_t offset;
    uint64_t total_bytes;
    const AmaterasuDataSegment *segments;
    uint32_t segment_count;
    uint32_t metadata_len;
    const uint8_t *metadata;
    const AmaterasuDataBufferLease *lease;
} AmaterasuDataChunk;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuDataChunk *chunk);
} AmaterasuDataChunkSink;
typedef struct {
    uint64_t generation;
    uint64_t queued_bytes;
    uint32_t queued_chunks;
    uint32_t phase;
} AmaterasuDataSnapshot;

/* Control creates/authorizes channels first. Each native access checks the
 * runtime-bound principal; first access claims one binding for the channel.
 * No broker/tick calls are needed on this data path. Calls may be concurrent.
 * Release only after all table calls finish; it retires every claimed channel.
 * Never reenter receive/cancel/release for this channel from a sink callback. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*describe)(void *, AmaterasuDataChannelId, AmaterasuDataDescription *);
    /* OK: whole chunk accepted locally. Not delivery. BUSY: retains/consumes
     * nothing. Null lease copies input, non-null lease retains it once. */
    AmaterasuStatus (*submit)(void *, AmaterasuDataChannelId, const AmaterasuDataChunk *);
    /* At most one synchronous callback. Non-OK preserves front for retry. */
    AmaterasuStatus (*receive)(void *, AmaterasuDataChannelId, AmaterasuDataChunkSink);
    AmaterasuStatus (*snapshot)(void *, AmaterasuDataChannelId, AmaterasuDataSnapshot *);
    /* Worker-only bounded wait on generation; timeout 0 takes a snapshot. */
    AmaterasuStatus (*wait)(void *, AmaterasuDataChannelId, uint64_t, uint32_t, AmaterasuDataSnapshot *);
    /* Stops admission; rejects unfinished records. Drains local queue only. */
    AmaterasuStatus (*finish)(void *, AmaterasuDataChannelId);
    /* OK certifies queued leases and in-flight sink calls have retired. */
    AmaterasuStatus (*cancel)(void *, AmaterasuDataChannelId);
} AmaterasuTransferApi;
#define AMATERASU_TRANSFER_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuTransferApi))

#ifdef __cplusplus
}
#endif
#endif
