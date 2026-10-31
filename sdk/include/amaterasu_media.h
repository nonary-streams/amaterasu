#ifndef AMATERASU_MEDIA_H
#define AMATERASU_MEDIA_H
#include "amaterasu.h"
#ifdef __cplusplus
extern "C" {
#endif
#define AMATERASU_PIXEL_BGRA8 1
#define AMATERASU_PIXEL_NV12 2
#define AMATERASU_CODEC_H264 1
#define AMATERASU_CODEC_HEVC 2
#define AMATERASU_CODEC_AV1 3
#define AMATERASU_PACKET_KEYFRAME 1
#define AMATERASU_PACKET_EOS 2
#define AMATERASU_PACKET_END_OF_ACCESS_UNIT 4
#define AMATERASU_MEDIA_PENDING 10
#define AMATERASU_MEDIA_BUSY 11
#define AMATERASU_MEDIA_CLOSED 12
#define AMATERASU_MEDIA_UNSUPPORTED 13
#define AMATERASU_MEDIA_BACKEND_ERROR 14
/* Typed native interface-family identities, distinct from service aliases.
 * Providers register their factory using native_register_typed with the matching
 * contract. Consumers use native_bind_typed with that identity before calling
 * the corresponding table; a matching size and version alone are insufficient.
 * The signed composition must grant the selected service and order its provider
 * before consumers. Native providers remain trusted code. */
/* Interface family identities for typed native registration and binding. */
#define AMATERASU_VIDEO_CAPTURE_CONTRACT "amaterasu.video-capture.native/1"
#define AMATERASU_VIDEO_ENCODER_CONTRACT "amaterasu.video-encoder.native/1"
#define AMATERASU_ENCODER_PROVIDER_CONTRACT "amaterasu.encoder-provider.native/1"
#define AMATERASU_FRAME_RESOURCE_CONSUMER_CONTRACT "amaterasu.frame-resource-consumer.native/1"
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_CONTRACT "amaterasu.frame-resource-processor.native/1"
#define AMATERASU_CPU_FRAME_PROCESSOR_CONTRACT "amaterasu.cpu-frame-processor.native/1"
#define AMATERASU_GPU_FRAME_PROCESSOR_CONTRACT "amaterasu.gpu-frame-processor.native/1"
#define AMATERASU_AUDIO_CAPTURE_CONTRACT "amaterasu.audio-capture.native/1"
#define AMATERASU_AUDIO_ENCODER_CONTRACT "amaterasu.audio-encoder.native/1"
#define AMATERASU_NEGOTIATED_AUDIO_ENCODER_CONTRACT "amaterasu.audio-encoder-negotiated.native/1"
#define AMATERASU_PCM_F32 1
#define AMATERASU_PCM_I16 2
#define AMATERASU_AUDIO_DISCONTINUITY 1
#define AMATERASU_AUDIO_SILENT 2
#define AMATERASU_AUDIO_TIMESTAMP_ESTIMATED 4
#define AMATERASU_CODEC_OPUS 2
#define AMATERASU_AUDIO_PACKET_DISCONTINUITY 1
#define AMATERASU_AUDIO_PACKET_EOS 2

/* Interleaved little-endian initialized PCM, at most 100 ms per callback. Silent packets
 * contain zero samples. Timestamps are stream-relative microseconds. */
typedef struct {
    uint32_t sample_rate, channels, format, frames, flags;
    const uint8_t *data;
    size_t len;
    uint64_t timestamp_us, sequence;
} AmaterasuAudioFrame;

typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuAudioFrame *frame);
} AmaterasuAudioSink;

typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*read_audio)(void *context, uint64_t stream, AmaterasuAudioSink sink);
} AmaterasuAudioCaptureApi;

/* One complete Opus packet. frames counts decoded samples per channel at 48 kHz.
 * Data is borrowed only during the callback. EOS has flags=2, frames=0, len=0.
 * Nonterminal packets have 120..5760 frames in multiples of 120, 1..8 channels,
 * and at most 65536 bytes. Mapping/order are negotiated through the control API. */
typedef struct {
    uint32_t codec, flags, sample_rate, channels, frames;
    const uint8_t *data;
    size_t len;
    uint64_t timestamp_us, sequence;
} AmaterasuEncodedAudioPacket;

typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuEncodedAudioPacket *packet);
} AmaterasuAudioPacketSink;

typedef struct {
    AmaterasuNativeInterface base;
    /* OK accepts all input samples; BUSY accepts none. */
    AmaterasuStatus (*submit)(void *context, uint64_t stream, const AmaterasuAudioFrame *frame);
    /* Rejected sink delivery retains the same packet for retry. */
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuAudioPacketSink sink);
    AmaterasuStatus (*drain)(void *context, uint64_t stream);
} AmaterasuAudioEncoderApi;

/* Independent negotiated encoder family. Existing Opus tables are unchanged.
 * Namespace zero denotes standard codecs; vendor namespaces require a separately
 * agreed schema. Valid structure does not imply codec or client support. Offers,
 * config bytes and packet bytes are borrowed only during synchronous callbacks. */
#define AMATERASU_AUDIO_FORMAT_VERSION 1u
#define AMATERASU_AUDIO_FORMAT_MAX_CONFIG_BYTES 4096u
#define AMATERASU_AUDIO_FORMAT_MAX_OFFERS 32u
#define AMATERASU_AUDIO_CODEC_OPUS 2u
#define AMATERASU_AUDIO_CODEC_AC3 3u
#define AMATERASU_AUDIO_CODEC_EAC3 4u
#define AMATERASU_AUDIO_FRAMING_OPUS_PACKET 1u
#define AMATERASU_AUDIO_FRAMING_AC3_SYNCFRAME 2u
#define AMATERASU_AUDIO_FRAMING_EAC3_SYNCFRAME 3u
typedef struct {
    uint32_t size, version;
    uint8_t namespace_id[16];
    uint32_t codec, framing, sample_rate, channels;
    uint64_t channel_mask;
    uint32_t frame_samples, reserved;
    AmaterasuBorrowedBytes config;
} AmaterasuAudioFormatDescriptor;
typedef struct { uint64_t id; AmaterasuAudioFormatDescriptor format; } AmaterasuAudioFormatOffer;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuAudioFormatOffer *);
} AmaterasuAudioFormatOfferSink;
typedef struct {
    uint64_t stream, generation, offer_id;
    uint32_t flags, frames;
    uint64_t timestamp_us, sequence;
    AmaterasuBorrowedBytes data;
} AmaterasuNegotiatedAudioPacket;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuNegotiatedAudioPacket *);
} AmaterasuNegotiatedAudioPacketSink;
/* Enumerate has unique nonzero offers and nonzero generation. configure accepts
 * an exact offered generation and ID or leaves selection unchanged. Rejection
 * preserves the packet for retry; drain stops admission and emits EOS. Stream
 * teardown retires pending work before the binding is released. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*enumerate)(void *, uint64_t, AmaterasuAudioFormatOfferSink, uint64_t *);
    AmaterasuStatus (*configure)(void *, uint64_t, uint64_t generation, uint64_t offer_id);
    AmaterasuStatus (*submit)(void *, uint64_t, const AmaterasuAudioFrame *);
    AmaterasuStatus (*receive)(void *, uint64_t, AmaterasuNegotiatedAudioPacketSink);
    AmaterasuStatus (*drain)(void *, uint64_t);
} AmaterasuNegotiatedAudioEncoderApi;

/* CPU bytes are borrowed only during submit or a synchronous frame callback.
 * NV12 is Y then UV, both with the same stride and even width/height. */
typedef struct {
    uint32_t width, height, format, stride;
    const uint8_t *data;
    size_t len;
    uint64_t timestamp_us, sequence;
} AmaterasuCpuFrame;

typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuCpuFrame *frame);
} AmaterasuFrameSink;

/* Optional synchronous CPU transform v1.0. Input and sink are borrowed until
 * process returns; output and bytes are borrowed only until write returns.
 * On OK invoke write exactly once; a processor failure before delivery invokes
 * it zero times. Return a rejected write status unchanged. BUSY consumes nothing and
 * may be retried. Preserve sequence and timestamp; shape and pixels may change.
 * Callbacks must be serialized and no pointer may be retained. */
#define AMATERASU_CPU_PROCESSOR_INTERFACE_MAJOR 1u
#define AMATERASU_CPU_PROCESSOR_INTERFACE_MINOR 0u
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*process)(void *context, uint64_t stream, const AmaterasuCpuFrame *frame, AmaterasuFrameSink sink);
} AmaterasuCpuProcessorApi;
#define AMATERASU_CPU_PROCESSOR_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuCpuProcessorApi))

/* Freshness is source provenance, not encoder acceptance: FRESH identifies a
 * newly acquired source frame; CACHED_REUSE identifies reused pixels. */
#define AMATERASU_CAPTURE_FRAME_CACHED_REUSE 0u
#define AMATERASU_CAPTURE_FRAME_FRESH 1u
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuCpuFrame *frame, uint32_t freshness);
} AmaterasuFreshFrameSinkV1_2;

/* Linux DMA-BUF frames. Every descriptor is borrowed only until the callback
 * or submit returns; a consumer that retains the pixels duplicates the fds
 * (F_DUPFD_CLOEXEC) and copies them out of the producer's buffer promptly.
 * fourcc/modifier are DRM values. Unused planes are {-1, -1, 0, 0}.
 * acquire_fence_fd is a sync_file signalled when GPU writes finish, or -1. */
#define AMATERASU_DMABUF_MAX_PLANES 4
#define AMATERASU_DMABUF_COLOR_SRGB 1u
#define AMATERASU_DMABUF_COLOR_BT2020_PQ 2u
typedef struct {
    int32_t fd, acquire_fence_fd;
    uint32_t offset, pitch;
} AmaterasuDmaBufPlane;

typedef struct {
    uint32_t width, height, fourcc, plane_count;
    uint64_t modifier;
    AmaterasuDmaBufPlane planes[AMATERASU_DMABUF_MAX_PLANES];
    uint32_t color, reserved;
    uint64_t timestamp_us, sequence;
} AmaterasuDmaBufFrame;

typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuDmaBufFrame *frame, uint32_t freshness);
} AmaterasuDmaBufFrameSink;

/* Borrowed lease over IMMUTABLE pixels. retain adds one reference on OK;
 * any error adds none. Providers impose finite outstanding-reference limits.
 * Every successful retain requires one release. A duplicated scanout fd alone
 * cannot implement this contract. Plane/acquire-fence fds stay provider-owned
 * and live until release; wait for acquire fences before reading.
 * release consumes one reference and, when >= 0, completion_fence_fd ownership.
 * The provider waits for the fence, closes it, and only then may reuse pixels.
 * -1 asserts all consumer GPU access completed. Invalid/failed fences must
 * quarantine storage. Callbacks/binding remain live through references AND
 * pending fences; callbacks run on the owning module thread and cannot unwind.
 * Returning from a sink does not implicitly retain or transfer a reference. */
typedef struct {
    AmaterasuDmaBufFrame frame;
    void *context;
    AmaterasuStatus (*retain)(void *context);
    void (*release)(void *context, int32_t completion_fence_fd);
} AmaterasuDmaBufFrameLease;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuDmaBufFrameLease *lease, uint32_t freshness);
} AmaterasuDmaBufLeaseSink;

/* Optional synchronous GPU transform. Input/output leases are borrowed;
 * retain explicitly for later use. On OK call the sink exactly once preserving
 * timestamp, sequence, and the supplied freshness. Propagate sink rejection unchanged. BUSY before
 * delivery consumes nothing. No callback may happen after process returns.
 * Processing cannot mutate input pixels and must honor all GPU fences. Finish
 * borrowed input reads before return or retain a separate reference and release
 * it after GPU completion/with a completion fence. Pending output writes require
 * acquire fences. The caller releasing its reference does not retire processor
 * work; the processor owns that independently retained reference. */
#define AMATERASU_GPU_PROCESSOR_INTERFACE_MAJOR 1u
#define AMATERASU_GPU_PROCESSOR_INTERFACE_MINOR 0u
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*process)(void *context, uint64_t stream, const AmaterasuDmaBufFrameLease *lease, uint32_t freshness, AmaterasuDmaBufLeaseSink sink);
    /* Stop stream permanently; synchronously retire all retained input/output
     * references and completion fences before return. May block on vendor/GPU
     * work. Source bindings must remain live throughout cancellation. */
    void (*cancel_dmabuf_leases)(void *context, uint64_t stream);
} AmaterasuGpuProcessorApi;
#define AMATERASU_GPU_PROCESSOR_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuGpuProcessorApi))

/* H264/HEVC bytes are Annex B; AV1 bytes are one OBU temporal unit. EOS has flags=2, len=0, no payload.
 * When open selects packet_boundaries=true, flag 4 marks the last fragment of
 * one picture. Its fragments share sequence/timestamp. EOS never carries 4. */
typedef struct {
    uint32_t codec, flags;
    const uint8_t *data;
    size_t len;
    uint64_t timestamp_us, sequence;
} AmaterasuEncodedPacket;

typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuEncodedPacket *packet);
} AmaterasuPacketSink;

/* Physical desktop pixels. Width and height are nonzero; the inclusive last
 * pixel on either axis must fit int32_t. Origins may be negative. */
typedef struct {
    int32_t left, top;
    uint32_t width, height;
} AmaterasuDesktopRect;

/* Output is contained in the virtual desktop. These are desktop coordinates,
 * independent of encoded frame dimensions and client viewport dimensions. */
typedef struct {
    AmaterasuDesktopRect output, virtual_desktop;
} AmaterasuCaptureGeometry;

/* Bind this prefix with minimum minor 0 to support older capture providers. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*read_frame)(void *context, uint64_t stream, AmaterasuFrameSink sink);
} AmaterasuCaptureApiPrefix;

#define AMATERASU_CAPTURE_INTERFACE_MINOR 1u
#define AMATERASU_CAPTURE_FRESHNESS_INTERFACE_MINOR 2u
#define AMATERASU_CAPTURE_PACING_TIMESTAMP_INTERFACE_MINOR 3u
#define AMATERASU_CAPTURE_DMABUF_INTERFACE_MINOR 4u
#define AMATERASU_CAPTURE_PRESENTATION_INTERFACE_MINOR 5u
#define AMATERASU_CAPTURE_LEASE_INTERFACE_MINOR 6u
#define AMATERASU_CAPTURE_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiPrefix))

typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*read_frame)(void *context, uint64_t stream, AmaterasuFrameSink sink);
    /* Optional v1.1 tail: check BOTH advertised minor >= 1 and table size >=
     * sizeof(AmaterasuCaptureApi) before accessing this member. The caller owns
     * writable output storage for this synchronous, owner-bound call. Only OK
     * initializes geometry; UNSUPPORTED means unavailable. */
    AmaterasuStatus (*geometry)(void *context, uint64_t stream, AmaterasuCaptureGeometry *geometry);
} AmaterasuCaptureApi;

/* Optional v1.2 tail: access only after checking advertised minor >= 2 and
 * table size >= sizeof(AmaterasuCaptureApiV1_2). The callback reports source
 * freshness for each borrowed frame without overloading sequence/timestamp. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*read_frame)(void *context, uint64_t stream, AmaterasuFrameSink sink);
    AmaterasuStatus (*geometry)(void *context, uint64_t stream, AmaterasuCaptureGeometry *geometry);
    AmaterasuStatus (*read_frame_with_freshness)(void *context, uint64_t stream, AmaterasuFreshFrameSinkV1_2 sink);
} AmaterasuCaptureApiV1_2;
#define AMATERASU_CAPTURE_API_V1_1_SIZE ((uint32_t)sizeof(AmaterasuCaptureApi))
#define AMATERASU_CAPTURE_API_V1_2_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_2))

/* Source pacing time has the capture stream's microsecond epoch; frame time
 * may instead be rewritten for encoder submission. Both are borrowed during
 * the synchronous callback. */
typedef struct {
    AmaterasuCpuFrame frame;
    uint64_t capture_pacing_timestamp_us;
} AmaterasuCaptureFrameV1_3;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuCaptureFrameV1_3 *frame, uint32_t freshness);
} AmaterasuFreshFrameSinkV1_3;

/* Each tail is available only when both the advertised minor and table size
 * cover that version. The older prefixes remain valid. */
#define AMATERASU_CAPTURE_V1_2_FIELDS \
    AmaterasuNativeInterface base; \
    AmaterasuStatus (*read_frame)(void *, uint64_t, AmaterasuFrameSink); \
    AmaterasuStatus (*geometry)(void *, uint64_t, AmaterasuCaptureGeometry *); \
    AmaterasuStatus (*read_frame_with_freshness)(void *, uint64_t, AmaterasuFreshFrameSinkV1_2)
#define AMATERASU_CAPTURE_V1_3_FIELDS \
    AMATERASU_CAPTURE_V1_2_FIELDS; \
    AmaterasuStatus (*read_frame_with_capture_timestamp)(void *, uint64_t, AmaterasuFreshFrameSinkV1_3)
#define AMATERASU_CAPTURE_V1_4_FIELDS \
    AMATERASU_CAPTURE_V1_3_FIELDS; \
    AmaterasuStatus (*read_dmabuf_frame)(void *, uint64_t, AmaterasuDmaBufFrameSink)
typedef struct { AMATERASU_CAPTURE_V1_3_FIELDS; } AmaterasuCaptureApiV1_3;
/* UNSUPPORTED means this stream has no DMA-BUF frames; use the CPU path. */
typedef struct { AMATERASU_CAPTURE_V1_4_FIELDS; } AmaterasuCaptureApiV1_4;
/* Sequence zero means no completed presentation. Timestamp uses stream epoch. */
typedef struct { uint64_t sequence, timestamp_us; } AmaterasuPresentationStatus;
typedef struct {
    AMATERASU_CAPTURE_V1_4_FIELDS;
    AmaterasuStatus (*latest_presentation)(void *, uint64_t, AmaterasuPresentationStatus *);
} AmaterasuCaptureApiV1_5;
/* The v1.5 prefix is unchanged. Only providers able to pin immutable contents
 * may expose leased capture. UNSUPPORTED permits the legacy borrowed path. */
typedef struct {
    AMATERASU_CAPTURE_V1_4_FIELDS;
    AmaterasuStatus (*latest_presentation)(void *, uint64_t, AmaterasuPresentationStatus *);
    AmaterasuStatus (*read_dmabuf_frame_leased)(void *, uint64_t, AmaterasuDmaBufLeaseSink);
} AmaterasuCaptureApiV1_6;
/* Optional v1.7 notifications. No pixels cross these callbacks. The consumer
 * context is live from successful subscribe until unsubscribe/control close
 * finishes. Callbacks may run on capture threads: record bounded metadata and
 * wake only; never reenter capture or encode inline. Subscribe announces any
 * already available frame/request. BUSY on request_swap accepts nothing.
 * Alias/reason: <=64 ASCII letters/digits/_/-/.; reason is nonempty. Empty alias
 * asks the consumer to choose. Output: optional <=512-byte UTF-8, no controls.
 * ACCEPTED approves a transition, not successful replacement capture startup. */
#define AMATERASU_CAPTURE_EVENTS_INTERFACE_MINOR 7u
#define AMATERASU_CAPTURE_SWAP_ACCEPTED 1u
#define AMATERASU_CAPTURE_SWAP_REJECTED 2u
typedef struct {
    uint64_t id;
    AmaterasuBorrowedBytes preferred_provider, output, reason;
} AmaterasuCaptureSwapRequest;
typedef struct {
    void *context;
    void (*frame_ready)(void *);
    AmaterasuStatus (*request_swap)(void *, const AmaterasuCaptureSwapRequest *);
    /* Terminal source failure; borrowed nonempty bounded reason code. */
    void (*stopped)(void *, AmaterasuBorrowedBytes reason);
} AmaterasuCaptureEventSink;
typedef struct {
    AmaterasuCaptureApiV1_6 base;
    AmaterasuStatus (*subscribe_events)(void *, uint64_t, AmaterasuCaptureEventSink);
    /* Mandatory, idempotent lifetime barrier, including after control close and
     * on binding release. No callback may still execute after return. */
    void (*unsubscribe_events)(void *, uint64_t);
    AmaterasuStatus (*resolve_swap)(void *, uint64_t, uint64_t, uint32_t, AmaterasuBorrowedBytes);
} AmaterasuCaptureApiV1_7;
#define AMATERASU_CAPTURE_API_V1_7_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_7))
/* Generic negotiated resources are additive to the legacy capture APIs. Native
 * modules are trusted; these grants do not sandbox native code. Kinds at or
 * above VENDOR_BASE require a separately agreed schema. Unknown kinds/versions
 * must be rejected. All descriptors and embedded handles are borrowed. */
#define AMATERASU_CAPTURE_RESOURCE_INTERFACE_MINOR 8u
#define AMATERASU_FRAME_RESOURCE_CONSUMER_INTERFACE_MAJOR 1u
#define AMATERASU_FRAME_RESOURCE_CONSUMER_INTERFACE_MINOR 0u
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_INTERFACE_MAJOR 1u
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_INTERFACE_MINOR 0u
/* Minor 1 opts in to synchronous retry-safe processor pipeline semantics. */
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_PIPELINE_MINOR 1u
#define AMATERASU_FRAME_RESOURCE_CPU 1u
#define AMATERASU_FRAME_RESOURCE_DMABUF 2u
#define AMATERASU_FRAME_RESOURCE_VENDOR_BASE 0x80000000u
#define AMATERASU_FRAME_SYNC_NONE 0u
#define AMATERASU_FRAME_SYNC_SYNC_FILE 1u
#define AMATERASU_FRAME_SYNC_VENDOR_BASE 0x80000000u
typedef struct {
    uint32_t kind, version, size, reserved;
    uint8_t namespace_id[16]; /* zero for standard kinds; nonzero UUID for vendor */
    const void *data;
} AmaterasuFrameDescriptor;
/* SYNC_FILE v1 points to this borrowed Linux sync_file descriptor. */
typedef struct { int32_t fd; } AmaterasuFrameSyncFile;
typedef struct {
    uint32_t format, min_width, max_width, min_height, max_height;
} AmaterasuCpuResourceCapabilities;
typedef struct {
    uint32_t fourcc, color;
    uint64_t modifier;
    uint32_t min_width, max_width, min_height, max_height;
    uint8_t device_id[16]; /* agreed UUID; zero means unknown */
} AmaterasuDmaBufResourceCapabilities;
#define AMATERASU_FRAME_OWNERSHIP_BORROWED 1u
#define AMATERASU_FRAME_OWNERSHIP_RETAINABLE 2u
#define AMATERASU_FRAME_ACCESS_CPU 1u
#define AMATERASU_FRAME_ACCESS_GPU 2u
/* IDs are nonzero and unique per stream/generation. Offer descriptors and
 * capability data are borrowed for the callback. CPU and DMA-BUF v1 offer
 * payloads are AmaterasuCpuResourceCapabilities and
 * AmaterasuDmaBufResourceCapabilities, distinct from frame payloads. Borrowed
 * ownership requires retain to return UNSUPPORTED. NONE has zero version/size
 * and NULL data. Schema-specific capability data must be checked for actual
 * device, format, and synchronization compatibility. */
typedef struct {
    uint64_t id;
    uint32_t ownership, accessibility, reserved;
    /* accessibility is a bitset. Zero is unspecified. CPU/GPU mean the schema
     * exposes a direct access/import mechanism, not physical residency, zero
     * copy, or absence of staging. Device/format/sync constraints still apply. */
    AmaterasuFrameDescriptor resource, acquire_sync, completion_sync;
} AmaterasuCaptureResourceOffer;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuCaptureResourceOffer *);
} AmaterasuCaptureResourceOfferSink;
/* Zero means unknown; never infer SDR or HDR from an unknown value. */
#define AMATERASU_FRAME_COLOR_UNKNOWN 0u
#define AMATERASU_FRAME_PRIMARIES_BT709 1u
#define AMATERASU_FRAME_PRIMARIES_BT2020 2u
#define AMATERASU_FRAME_TRANSFER_SRGB 1u
#define AMATERASU_FRAME_TRANSFER_BT709 2u
#define AMATERASU_FRAME_TRANSFER_PQ 3u
#define AMATERASU_FRAME_TRANSFER_HLG 4u
#define AMATERASU_FRAME_MATRIX_RGB 1u
#define AMATERASU_FRAME_MATRIX_BT709 2u
#define AMATERASU_FRAME_MATRIX_BT2020_NCL 3u
#define AMATERASU_FRAME_RANGE_FULL 1u
#define AMATERASU_FRAME_RANGE_LIMITED 2u
#define AMATERASU_FRAME_CLOCK_UNKNOWN 0u
#define AMATERASU_FRAME_CLOCK_STREAM_RELATIVE 1u
#define AMATERASU_FRAME_CLOCK_HOST_MONOTONIC 2u
/* All zero means absent. In v1 size is sizeof the record. Each zero color
 * component remains independently unknown. Unknown versions/values must not
 * be interpreted as known color. Timestamps are in microseconds in their
 * respective clocks; only compare understood domains with matching nonzero epoch
 * IDs. A zero epoch is unknown and does not establish comparability. */
typedef struct {
    uint32_t version, size, primaries, transfer, matrix, range;
} AmaterasuFrameColorMetadata;
typedef struct {
    uint32_t version, size, domain, reserved;
    uint8_t epoch_id[16];
} AmaterasuFrameClockMetadata;
/* CPU resource v1 points to AmaterasuCpuFrame; DMA-BUF v1 points to
 * AmaterasuDmaBufFrame. Descriptor size must cover that struct. CPU uses
 * SYNC_NONE; DMA-BUF acquire fences remain in its planes and must be waited.
 * Copy frame and descriptors during callback. Successful retain pins immutable
 * pixels, descriptor data and handles through release; failed retain acquires
 * nothing. Providers impose finite reference limits. Each release consumes one
 * retained reference; NONE asserts all work completed. Otherwise completion
 * covers all work and is borrowed only until release returns: provider must
 * duplicate/retain handles synchronously and quarantine on failure. Keep provider
 * and binding live through all references and pending work. Selection changes
 * and stream close do not invalidate retained frames; destruction waits for
 * references and completion fences. Callbacks run on
 * the owning module thread and must not unwind across the ABI. */
typedef struct {
    uint64_t offer_generation, offer_id, timestamp_us, capture_pacing_timestamp_us, sequence;
    uint32_t width, height, freshness, reserved;
    AmaterasuFrameColorMetadata color;
    AmaterasuFrameClockMetadata clock, pacing_clock;
    AmaterasuFrameDescriptor resource, acquire_sync;
    void *context;
    AmaterasuStatus (*retain)(void *);
    void (*release)(void *, const AmaterasuFrameDescriptor *completion);
} AmaterasuCaptureResourceFrame;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuCaptureResourceFrame *);
} AmaterasuCaptureResourceFrameSink;
/* AFM1 is a bounded pointer-free metadata envelope. Schema IDs are triples;
 * value 4 identifies the bundle in EncoderFrameOptionsV1.side_data_schema.
 * Values 1 and 2 identify raw complete ITU-T T.35 HDR10+/HDR Vivid payloads,
 * not codec-wrapped units. Schema identities are interpreted in their context. */
#define AMATERASU_ENCODER_SCHEMA_FRAME_METADATA_BUNDLE 4u
#define AMATERASU_FRAME_METADATA_BUNDLE_VERSION 1u
#define AMATERASU_FRAME_METADATA_MAX_ITEMS 16u
#define AMATERASU_FRAME_METADATA_MAX_ITEM_BYTES (16u * 1024u)
#define AMATERASU_FRAME_METADATA_MAX_BUNDLE_BYTES (64u * 1024u)
#define AMATERASU_FRAME_METADATA_ITEM_REQUIRED 1u
#define AMATERASU_FRAME_METADATA_OWNER_ENCODER 1u
#define AMATERASU_FRAME_METADATA_OWNER_FRAMER 2u
#define AMATERASU_FRAME_METADATA_SCHEMA_HDR10PLUS_T35 1u
#define AMATERASU_FRAME_METADATA_SCHEMA_HDR_VIVID_T35 2u
#define AMATERASU_FRAME_METADATA_CAP_FORWARD 1u
#define AMATERASU_FRAME_METADATA_CAP_APPLY 2u
#define AMATERASU_FRAME_METADATA_CAPABILITIES_CONTRACT "amaterasu.frame-metadata-capabilities.native/1"
typedef struct { uint64_t namespace_id, value, schema; } AmaterasuFrameMetadataSchemaId;
typedef struct { AmaterasuFrameMetadataSchemaId schema; uint32_t operations, reserved; } AmaterasuFrameMetadataCapabilityV1;
typedef struct { void *context; AmaterasuStatus (*write)(void *, const AmaterasuFrameMetadataCapabilityV1 *); } AmaterasuFrameMetadataCapabilitySinkV1;
/* Enumerates session-scoped capability claims synchronously; trusted
 * composition, not the provider, chooses owner. Generations are nonzero. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*enumerate)(void *, uint64_t, AmaterasuFrameMetadataCapabilitySinkV1, uint64_t *generation);
} AmaterasuFrameMetadataCapabilitiesApiV1;
#define AMATERASU_FRAME_METADATA_CAPABILITIES_API_V1_SIZE ((uint32_t)sizeof(AmaterasuFrameMetadataCapabilitiesApiV1))
#define AMATERASU_CAPTURE_METADATA_INTERFACE_MINOR 10u
#define AMATERASU_FRAME_RESOURCE_CONSUMER_METADATA_MINOR 1u
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_METADATA_MINOR 2u
/* metadata is borrowed only for this synchronous call. Retaining frame does
 * not retain it: copy the validated bounded envelope before callback return. */
typedef struct {
    uint32_t size, version;
    AmaterasuCaptureResourceFrame frame;
    AmaterasuBorrowedBytes metadata;
} AmaterasuFrameResourceMetadataFrameV1;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuFrameResourceMetadataFrameV1 *);
} AmaterasuFrameResourceMetadataSinkV1;
#define AMATERASU_FRAME_RESOURCE_METADATA_FRAME_V1_SIZE ((uint32_t)sizeof(AmaterasuFrameResourceMetadataFrameV1))
/* Enumeration is synchronous and returns a nonzero generation even for an
 * empty list. Selection uses that exact generation/ID; stale or unavailable
 * returns UNSUPPORTED without changing the prior selection. Capability changes
 * invalidate it. Reads deliver at most one frame, PENDING none; sink errors
 * propagate. Legacy read operations remain available. Calls require the same
 * authenticated stream ownership as legacy operations. The provider keeps an
 * initial frame reference during the sink callback; sink return transfers none. */
typedef struct {
    AmaterasuCaptureApiV1_7 base;
    AmaterasuStatus (*enumerate_resources)(void *, uint64_t, AmaterasuCaptureResourceOfferSink, uint64_t *generation);
    AmaterasuStatus (*select_resource)(void *, uint64_t, uint64_t generation, uint64_t offer_id);
    AmaterasuStatus (*read_resource_frame)(void *, uint64_t, AmaterasuCaptureResourceFrameSink);
} AmaterasuCaptureApiV1_8;
#define AMATERASU_CAPTURE_API_V1_8_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_8))
#define AMATERASU_FRAME_RESOURCE_SUBMIT_FORCE_IDR 1u
/* An independently bound consumer (e.g. encoder). Its offer IDs and generation
 * belong to its own stream. Configure verifies the producer offer and both
 * schemas; incompatible/stale returns UNSUPPORTED with configuration unchanged.
 * Producer generation and offer are borrowed during configure; copy needed
 * data before returning. Configure first, then select the producer; if select
 * fails, do not submit and re-enumerate/reconfigure. Submit's final
 * flags FORCE_IDR force codec recovery or return UNSUPPORTED without consuming;
 * unknown bits also return UNSUPPORTED without consuming. Submit finishes access
 * before returning or retains for asynchronous work; failed submit retains no
 * work. Cancel stops admission and retires retained references and work. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*enumerate_resources)(void *, uint64_t, AmaterasuCaptureResourceOfferSink, uint64_t *generation);
    AmaterasuStatus (*configure)(void *, uint64_t, uint64_t consumer_generation, uint64_t consumer_id, uint64_t producer_generation, const AmaterasuCaptureResourceOffer *producer_offer);
    AmaterasuStatus (*submit)(void *, uint64_t, const AmaterasuCaptureResourceFrame *, uint32_t flags);
    void (*cancel)(void *, uint64_t);
} AmaterasuFrameResourceConsumerApi;
/* The processor follows the same lifetime rules; process delivers at most one
 * output synchronously, preserving sequence, timestamps and freshness.
 * Minor 1 propagates the sink status exactly; non-OK consumes no input and
 * retains no work or output. OK without a callback consumes input with no output.
 * Its callback runs on the calling thread; output preserves both clock descriptors
 * and can be submitted to another provider before process returns. Minor 0 does
 * not promise these retry-safe pipeline semantics. */
typedef struct {
    AmaterasuFrameResourceConsumerApi base;
    AmaterasuStatus (*enumerate_outputs)(void *, uint64_t, AmaterasuCaptureResourceOfferSink, uint64_t *generation);
    AmaterasuStatus (*select_output)(void *, uint64_t, uint64_t generation, uint64_t offer_id);
    AmaterasuStatus (*process)(void *, uint64_t, const AmaterasuCaptureResourceFrame *, AmaterasuCaptureResourceFrameSink);
} AmaterasuFrameResourceProcessorApi;
#define AMATERASU_FRAME_RESOURCE_CONSUMER_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuFrameResourceConsumerApi))
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuFrameResourceProcessorApi))
/* Optional additive tails. The selected trusted composition fixes one owner
 * and schema policy; a provider's envelope is never authority for that choice.
 * Non-OK consumes neither frame nor metadata. Cancel retires both; async users
 * copy metadata independently of a retained frame. Processor output preserves
 * session, submission, source/generations, timestamp and clock identity. */
typedef struct {
    AmaterasuFrameResourceConsumerApi base;
    AmaterasuStatus (*submit_with_metadata)(void *, uint64_t, const AmaterasuFrameResourceMetadataFrameV1 *, uint32_t);
} AmaterasuFrameResourceConsumerApiV1_1;
typedef struct {
    AmaterasuFrameResourceProcessorApi base;
    AmaterasuStatus (*process_with_metadata)(void *, uint64_t, const AmaterasuFrameResourceMetadataFrameV1 *, AmaterasuFrameResourceMetadataSinkV1);
} AmaterasuFrameResourceProcessorApiV1_2;
#define AMATERASU_FRAME_RESOURCE_CONSUMER_API_V1_1_SIZE ((uint32_t)sizeof(AmaterasuFrameResourceConsumerApiV1_1))
#define AMATERASU_FRAME_RESOURCE_PROCESSOR_API_V1_2_SIZE ((uint32_t)sizeof(AmaterasuFrameResourceProcessorApiV1_2))
#undef AMATERASU_CAPTURE_V1_4_FIELDS
#undef AMATERASU_CAPTURE_V1_3_FIELDS
#undef AMATERASU_CAPTURE_V1_2_FIELDS
#define AMATERASU_CAPTURE_API_V1_3_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_3))
#define AMATERASU_CAPTURE_API_V1_4_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_4))
#define AMATERASU_CAPTURE_API_V1_5_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_5))
#define AMATERASU_CAPTURE_API_V1_6_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_6))

typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit)(void *context, uint64_t stream, const AmaterasuCpuFrame *frame, uint32_t force_idr);
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
    AmaterasuStatus (*drain)(void *context, uint64_t stream);
} AmaterasuEncoderApi;

/* Optional encoder v1.1 tail: access only after checking advertised minor >= 1
 * and table size >= sizeof(AmaterasuEncoderApiV1_1). OK means the encoder has
 * duplicated what it needs; BUSY accepts nothing; UNSUPPORTED means this
 * stream cannot import the frame's format or modifier. */
#define AMATERASU_ENCODER_DMABUF_INTERFACE_MINOR 1u
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit)(void *context, uint64_t stream, const AmaterasuCpuFrame *frame, uint32_t force_idr);
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
    AmaterasuStatus (*drain)(void *context, uint64_t stream);
    AmaterasuStatus (*submit_dmabuf)(void *context, uint64_t stream, const AmaterasuDmaBufFrame *frame, uint32_t force_idr);
} AmaterasuEncoderApiV1_1;
/* Leased encoder v1.2: asynchronous work must retain before returning OK
 * and release only after safe GPU access completes (or with completion fence).
 * Every non-OK return retains nothing and retires all GPU access started by the
 * rejected submission before returning. Drain completion retires all accepted
 * references. Provider code/binding must outlive pending completion fences. */
#define AMATERASU_ENCODER_LEASE_INTERFACE_MINOR 2u
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit)(void *, uint64_t, const AmaterasuCpuFrame *, uint32_t);
    AmaterasuStatus (*receive)(void *, uint64_t, AmaterasuPacketSink);
    AmaterasuStatus (*drain)(void *, uint64_t);
    AmaterasuStatus (*submit_dmabuf)(void *, uint64_t, const AmaterasuDmaBufFrame *, uint32_t);
    AmaterasuStatus (*submit_dmabuf_leased)(void *, uint64_t, const AmaterasuDmaBufFrameLease *, uint32_t);
    /* Permanently stop this stream; synchronously retire all accepted input
     * references AND completion fences before returning. May block on vendor
     * work. Capture/processor bindings stay live during cancellation. Failure
     * safely quarantines storage; no pending access/reference may remain. */
    void (*cancel_dmabuf_leases)(void *, uint64_t);
} AmaterasuEncoderApiV1_2;
#define AMATERASU_ENCODER_API_V1_2_SIZE ((uint32_t)sizeof(AmaterasuEncoderApiV1_2))
#define AMATERASU_ENCODER_API_V1_0_SIZE ((uint32_t)sizeof(AmaterasuEncoderApi))
#define AMATERASU_ENCODER_API_V1_1_SIZE ((uint32_t)sizeof(AmaterasuEncoderApiV1_1))
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
} AmaterasuVideoApi;
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
    /* OK initializes geometry; UNSUPPORTED means unavailable. */
    AmaterasuStatus (*geometry)(void *context, uint64_t stream, AmaterasuCaptureGeometry *geometry);
} AmaterasuVideoApiV1_1;

/* Register and bind each transport using its exact typed native family.
 * Untyped factories and other families are rejected for every service alias. */
#define AMATERASU_MEDIA_TRANSPORT_CONTRACT "amaterasu.media-transport.native/1"
#define AMATERASU_CONTROL_TRANSPORT_CONTRACT "amaterasu.control-transport.native/1"

/* Video providers register and consumers bind this typed family for every alias. */
#define AMATERASU_VIDEO_CONTRACT "amaterasu.video.native/1"

/* The trusted control caller must authorize the peer and negotiate the profile.
 * The provider enforces ownership; it does not authenticate peers. Submit one
 * complete H.264 Annex B access unit or one negotiated Opus packet per call.
 * Bytes are borrowed for each call. OK means queued, never peer delivery.
 * BUSY accepts nothing and consumes no packet indices/nonces. Drain stops input;
 * control status reports completion of local queue flushing, not peer receipt. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit_video)(void *context, uint64_t stream, const AmaterasuEncodedPacket *packet);
    AmaterasuStatus (*submit_audio)(void *context, uint64_t stream, const AmaterasuEncodedAudioPacket *packet);
    AmaterasuStatus (*drain)(void *context, uint64_t stream);
} AmaterasuMediaTransportApi;
/* Logical control messages. Sequence is authenticated receive metadata and is
 * ignored on send; providers always choose their own encryption nonces. */
#define AMATERASU_CONTROL_RELIABLE 1u
#define AMATERASU_CONTROL_UNRELIABLE 2u
#define AMATERASU_CONTROL_UNSEQUENCED 3u
typedef struct {
    uint32_t message_type, channel, delivery, reserved;
    const uint8_t *data;
    size_t len;
    uint64_t sequence;
} AmaterasuControlMessage;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *context, const AmaterasuControlMessage *message);
} AmaterasuControlMessageSink;
/* Bytes remain borrowed until each call returns. BUSY accepts nothing and
 * consumes no nonce. Receive calls the sink at most once and retains a rejected
 * message. The control service establishes stream ownership and fresh keys. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*send)(void *context, uint64_t stream, const AmaterasuControlMessage *message);
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuControlMessageSink sink);
} AmaterasuControlTransportApi;
/* Negotiated video unit/framing contracts are separate from legacy VideoApi
 * and MediaTransportApi. Codec IDs match the encoder-provider triple; zero is
 * invalid. Buffers are borrowed synchronously and BUSY consumes nothing. */
#define AMATERASU_NEGOTIATED_VIDEO_CONTRACT "amaterasu.video-unit.native/1"
#define AMATERASU_NEGOTIATED_VIDEO_TRANSPORT_CONTRACT "amaterasu.video-unit-transport.native/1"
#define AMATERASU_VIDEO_FRAMING_CONTRACT "amaterasu.video-framing.native/2"
#define AMATERASU_VIDEO_UNIT_VERSION 1u
#define AMATERASU_VIDEO_UNIT_KEYFRAME 1u
#define AMATERASU_VIDEO_UNIT_DISCONTINUITY 2u
#define AMATERASU_VIDEO_UNIT_END_OF_STREAM 4u
#define AMATERASU_VIDEO_FRAME_MAX_BLOCKS 4u
#define AMATERASU_VIDEO_FRAME_MAX_DATAGRAMS 2048u
#define AMATERASU_VIDEO_FRAME_RECORD_START 1u
#define AMATERASU_VIDEO_FRAME_CRITICAL_PREFIX 2u
#define AMATERASU_VIDEO_FRAME_ALLOW_LARGE_UNPROTECTED 1u
#define AMATERASU_VIDEO_FRAMING_LIMITS_VERSION 2u
#define AMATERASU_VIDEO_FRAMING_INTERFACE_MAJOR 2u
#define AMATERASU_VIDEO_FRAMING_INTERFACE_MINOR 0u
typedef struct { uint64_t namespace_id, value, schema; } AmaterasuVideoFormatId;
typedef struct {
    uint32_t size, version;
    uint64_t stream, generation, offer_id;
    AmaterasuVideoFormatId codec, framing;
    uint64_t access_unit_id, submission_id, source_id, config_generation, timestamp_us;
    AmaterasuFrameClockMetadata clock;
    uint32_t flags, reserved;
    AmaterasuBorrowedBytes data;
} AmaterasuNegotiatedVideoUnit;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuNegotiatedVideoUnit *);
} AmaterasuNegotiatedVideoUnitSink;
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*receive)(void *, uint64_t, AmaterasuNegotiatedVideoUnitSink);
} AmaterasuNegotiatedVideoApi;
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit)(void *, uint64_t, const AmaterasuNegotiatedVideoUnit *);
    AmaterasuStatus (*drain)(void *, uint64_t);
} AmaterasuNegotiatedVideoTransportApi;
typedef struct {
    uint32_t first_data_shard, data_shards, parity_shards, percentage;
} AmaterasuVideoFecBlock;
typedef struct {
    uint32_t size, version, packet_size, crypto_overhead, network_overhead;
    uint32_t allowed_extensions, max_datagrams, max_queue_bytes;
    uint32_t min_parity_shards, max_fec_percentage;
    uint64_t max_wire_bytes;
    /* V2 tail; the distinct framing v2 contract is required. */
    uint32_t min_fec_percentage, max_fec_blocks, fec_policy_flags, reserved;
} AmaterasuVideoFramingLimits;
typedef struct {
    uint32_t size, version;
    AmaterasuBorrowedBytes framed_bytes, record_starts;
    uint32_t critical_prefix_shards, block_count;
    AmaterasuVideoFecBlock blocks[AMATERASU_VIDEO_FRAME_MAX_BLOCKS];
} AmaterasuVideoFramePlan;
typedef struct {
    void *context;
    AmaterasuStatus (*write)(void *, const AmaterasuVideoFramePlan *);
} AmaterasuVideoFramePlanSink;
/* The host validates plan bounds and negotiated flags, then owns FEC,
 * sequence numbers, encryption and network delivery. The plan callback
 * must not retain pointers or commit rate state; retry after BUSY is safe. */
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*plan)(void *, uint64_t, const AmaterasuNegotiatedVideoUnit *,
                            const AmaterasuVideoFramingLimits *, AmaterasuVideoFramePlanSink);
} AmaterasuVideoFramingApi;

/* HDR metadata is tied to a completed capture presentation. Fields: RGB xy,
 * white xy (1/50000), max nits, min 1/10000 nit, MaxCLL and MaxFALL. */
typedef struct { uint32_t enabled; uint16_t fields[12]; uint32_t reserved; } AmaterasuHdrMetadata;
typedef struct {
    AmaterasuCaptureApiV1_8 base;
    AmaterasuStatus (*hdr_metadata)(void *, uint64_t, AmaterasuHdrMetadata *);
} AmaterasuCaptureApiV1_9;
/* Optional capture tail; missing tail cannot justify dropping required data. */
typedef struct {
    AmaterasuCaptureApiV1_9 base;
    AmaterasuStatus (*read_resource_frame_with_metadata)(void *, uint64_t, AmaterasuFrameResourceMetadataSinkV1);
} AmaterasuCaptureApiV1_10;
#define AMATERASU_CAPTURE_API_V1_10_SIZE ((uint32_t)sizeof(AmaterasuCaptureApiV1_10))
typedef struct {
    AmaterasuVideoApiV1_1 base;
    AmaterasuStatus (*hdr_metadata)(void *, uint64_t, AmaterasuHdrMetadata *);
} AmaterasuVideoApiV1_2;
#ifdef __cplusplus
}
#endif
#endif
