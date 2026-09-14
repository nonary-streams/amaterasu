#ifndef AMATERASU_MEDIA_H
#define AMATERASU_MEDIA_H
#include "amaterasu.h"
#ifdef __cplusplus
extern "C" {
#endif
#define AMATERASU_PIXEL_BGRA8 1
#define AMATERASU_PIXEL_NV12 2
#define AMATERASU_CODEC_H264 1
#define AMATERASU_PACKET_KEYFRAME 1
#define AMATERASU_PACKET_EOS 2
#define AMATERASU_PACKET_END_OF_ACCESS_UNIT 4
#define AMATERASU_MEDIA_PENDING 10
#define AMATERASU_MEDIA_BUSY 11
#define AMATERASU_MEDIA_CLOSED 12
#define AMATERASU_MEDIA_UNSUPPORTED 13
#define AMATERASU_MEDIA_BACKEND_ERROR 14
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

/* H264 bytes are Annex B. EOS has flags=2, len=0, no payload.
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

typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*submit)(void *context, uint64_t stream, const AmaterasuCpuFrame *frame, uint32_t force_idr);
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
    AmaterasuStatus (*drain)(void *context, uint64_t stream);
} AmaterasuEncoderApi;
typedef struct {
    AmaterasuNativeInterface base;
    AmaterasuStatus (*receive)(void *context, uint64_t stream, AmaterasuPacketSink sink);
} AmaterasuVideoApi;

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
#ifdef __cplusplus
}
#endif
#endif
