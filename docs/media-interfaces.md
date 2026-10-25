# Draft native media interfaces

Control commands select resources and manage their lifetime. CPU frames and encoded
packets cross versioned C tables through synchronous callbacks; they never become
JSON arrays, raw JSON pointers to media data, or Rust objects shared across DLLs.
See [native interfaces](native-interfaces.md) for binding authority and lifetime.
The public definitions are in `sdk/include/amaterasu_media.h` and `crates/media`.

Capture, encoder, video and transport tables each begin with `AmaterasuNativeInterface`.
The runtime binds the consuming module's authenticated principal; providers check
that principal owns the numeric stream/session ID on every media operation.
`native_use` grants are separate from ordinary control `call` grants. In particular,
the development console gets no native pointer access from its control permissions.

CPU format 1 is BGRA8. Format 2 is contiguous NV12 with Y followed by interleaved UV,
using the same stride and even dimensions. Buffers include exactly stride times
plane rows; metadata validation bounds each allocation to 128 MiB and dimensions
to 8192 per axis. Validation alone cannot prove pointer provenance: trusted native
callers must supply real live initialized allocations. Capture copies tightly
packed BGRA rows, avoiding mapped GPU row padding. Timestamps are microseconds
from a producer-defined stream epoch, with a per-stream sequence number.

Encoder submit returns success only after accepting/copying the frame. Busy accepts
nothing. Encoded H.264 packets use Annex B start codes, with keyframe metadata;
packets are bounded to 16 MiB. Receiving calls a sink at most once. A rejected sink
must not consume the queued packet. Drain stops new input and produces queued output
followed by a single empty end-of-stream packet. The video table exposes encoded
packet receive, while open/close/configuration stay on its control service.

New consumers request `packet_boundaries: true` when opening video or an encoder
and verify the selected mode in the response. Only opted-in streams emit flag 4
(`END_OF_ACCESS_UNIT`) on the final fragment of a complete picture. Fragments
share sequence and timestamp; EOS remains an empty packet with flag 2 alone.
Older consumers retain their original flags without changing the table layout.
The [stream-session library](../modules/stream-session/README.md) uses this contract
to assemble bounded access units before transport submission.

Callbacks borrow data only until they return. A consumer must copy anything it
retains. No allocator or `Vec` ownership crosses the ABI. Interfaces must outlive
all calls; consumers release their bindings before provider DLLs unload. Factory
descriptors are copied at registration; their code/context still need stable
lifetimes. Bound product tables remain provider-owned until release.

## DMA-BUF frames

Capture v1.4 (`CaptureApiV1_4.read_dmabuf_frame`) and encoder v1.1
(`EncoderApiV1_1.submit_dmabuf`) carry Linux GPU frames as `DmaBufFrame`: DRM
fourcc and modifier, one to four planes with fd, offset, pitch and an optional
acquire sync_file, explicit color (`SRGB` or `BT2020_PQ`), timestamp and sequence.
Both tails are additive; consumers check the advertised minor and table size
before reading them, and fall back to CPU frames on `MEDIA_UNSUPPORTED`.
Descriptors are borrowed for the call exactly like CPU bytes. An encoder that
accepts a frame duplicates the fds before returning, waits for the acquire fences
on its own GPU timeline, and copies the pixels into storage it owns before the
next presentation can reuse the producer's buffer. Keeping a DMA-BUF open pins the
allocation; it does not stop the compositor from rendering into it again.

Capture v1.5 adds `latest_presentation`, a non-blocking query of the newest
completed presentation (`PresentationStatus { sequence, timestamp_us }`). A
consumer polls it each tick and reads a frame only when the sequence advances,
so frames are pushed at the compositor's presentation rate, including variable
refresh, rather than sampled on a fixed timer.

Encoded video leaving the video service is stamped on the shared host
monotonic clock (`amaterasu_media::host_monotonic_micros`: CLOCK_MONOTONIC on
Linux, the performance counter on Windows) at capture or presentation. The media
transport compares it with the same clock at packetization and reports the
difference, in 0.1 ms units, as the GameStream frame header's host processing
latency that Moonlight shows. A timestamp in the future or over five seconds old
is reported as unknown (zero).

Codec 3 is AV1: each packet is one low-overhead OBU temporal unit, starting with
a temporal delimiter. AV1 has no start codes, so transports frame it by length.

These interfaces are draft. CPU readback and conversion define the initial
integration boundary, but GPU texture interop, codec capability negotiation,
HDR/color metadata, cursor composition, rotation, Linux capture/encoding, and
physical capture-to-encode qualification remain necessary before freezing it.
The native interface mechanism does not sandbox trusted module code.

Validation includes Rust frame/packet bounds tests, C header layout assertions
(`cc -std=c11 -Wall -Wextra -Werror -fsyntax-only -I sdk/include sdk/tests/media_layout.c`),
and `scripts/test_native_media.py` for signed-library registration/binding with
hardware disabled. These checks do not establish Moonlight or WebRTC streaming.

## PCM audio capture

`AudioCaptureApi` delivers interleaved PCM through an `AudioSink`. Each `AudioFrame`
contains sample rate, channel count, format, frame count, flags, initialized bytes,
stream-relative microsecond timestamp and sequence. Format 1 is 32-bit IEEE float;
format 2 is signed 16-bit PCM. Both PCM formats use explicit little-endian ordering.
Control format metadata supplies the channel mask; channel count alone does not
identify a surround layout.

A delivery spans at most 100 ms, with 8–192 kHz rates and one to eight channels.
The exact byte count must match frames × channels × bytes per sample and remain
under 1 MiB. Silent packets still carry initialized zero samples. Flags distinguish
discontinuity, silence and an estimated timestamp, allowing consumers to reset
continuity assumptions. The sink borrows samples only until returning; rejection
must retain the packet for retry. These bounds constrain individual deliveries,
not the total buffering policy of a provider.

## Encoded audio

`AudioEncoderApi` accepts borrowed PCM and delivers `EncodedAudioPacket` through
an `AudioPacketSink`. Encoded packets carry codec, rate, channels, decoded frame
count, discontinuity/EOS flags, timestamp and sequence alongside borrowed bytes.
They use a separate structure from video packets so audio timing does not rely
on implicit codec-specific metadata. Rejected sink deliveries stay queued.

The current Opus provider accepts 48 kHz mono/stereo PCM, encodes fixed 5/10/20 ms
packets and reports its actual constant-bitrate packet size in control metadata.
Drain flushes buffered PCM and codec lookahead before one EOS. Timing drift is
reported; resampling and audiovisual synchronization belong to future coordination.
See the [encoder contract](../modules/audio-encode/README.md) for queue limits,
partial-packet handling and ownership.

## Encoded media transport

`MediaTransportApi` exposes `submit_video`, `submit_audio` and `drain` for an owned
transport stream. The trusted control caller must first authorize its peer and
negotiate the wire profile. The transport checks caller ownership but does not
perform peer authentication. It accepts one complete H.264 Annex B access unit or
one complete negotiated Opus packet per submission. Consumers must assemble any
video encoder fragments before submitting them.

Success means copied into a bounded local queue. Busy means nothing was accepted,
and no packet indices or nonces were consumed. Drain stops both inputs and begins
flushing queued datagrams; control status reports local completion. It does not
assert client receipt, successful decoding or an invented network EOS marker.
See the [transport contract](../modules/media-transport/README.md) for FEC profiles,
crypto lifetimes, explicit legacy audio IV policy and failure behavior.

## Authenticated control messages

`ControlTransportApi` sends and receives complete logical GameStream messages
through `ControlMessage`. Payloads remain borrowed during each native call;
received plaintext is authenticated by the transport before delivery. Type/body
meaning is separate from ENet channel and delivery metadata, which are outside
GCM authentication and must not be treated as authority.

A receive sink may reject a delivery without consuming it. Send checks bounded
admission before assigning encryption sequence numbers; the caller cannot choose
or rewind nonces. Keys, destination admission and stream creation stay on the
trusted control service. See the
[control transport contract](../modules/control-transport/README.md) for ownership,
liveness, replay and wire-compatibility limits.
