# GameStream protocol library

`amaterasu-gamestream` contains protocol framing, media packetization and encryption.
RTSP, SDP and packetization use the Rust standard library. Encryption uses ring
0.17.14, AES 0.8.4 with zeroization enabled, and RustCrypto CBC 0.1.2; cipher and
block-padding support come through those dependencies. Cryptographic primitives
are not reimplemented here. The crate contains no local unsafe code, sockets,
runtime grants or device access; dependencies have their own native/unsafe code.
Managed libraries own sessions, authorization, resource allocation, transport and
cleanup. Valid protocol data is not proof that a stream can start.

## RTSP

`rtsp::Decoder::push` accepts bounded input; repeatedly call `next_request` until
it returns `None`, then read another chunk. Requests own their body, target and
normalized headers. `finish` detects truncated input after complete requests have
been drained at EOF. Any framing error poisons the decoder; close the transport
instead of searching for another message boundary.

The parser accepts RTSP/1.0 OPTIONS, DESCRIBE, SETUP, ANNOUNCE, PLAY and TEARDOWN.
Targets remain opaque, accommodating absolute RTSP URLs, `streamid=...` and `/`.
The caller must validate targets, Host, Session, Transport, content types and
method ordering against its actual session. CSeq is required once and parsed as
an unsigned 32-bit decimal; duplicate/replay policy across requests belongs to
that session, not a connection's framing parser.

Limits are fixed: 16 KiB headers, 64 KiB body, 64 headers, 2048 bytes per line,
and 160 KiB buffered input. Callers should drain between bounded reads rather
than append an arbitrary number of coalesced requests. Header scanning resumes
at the prior boundary; a parsed header is retained while its body arrives.
Content-Length is one unsigned decimal value, with an absent header meaning a
zero-byte body. Body bytes are opaque and may contain apparent RTSP delimiters.

Only CRLF lines are supported. Every duplicate header name is rejected, even if
its value agrees. Whitespace before a colon, folded headers, non-ASCII header
bytes, control characters, Transfer-Encoding, Trailer and Content-Encoding are
rejected. These deliberate restrictions avoid competing framing interpretations;
this is not a general permissive RFC client/server implementation. RTP-over-TCP
interleaving, encrypted RTSP envelopes and legacy ENet framing are not implemented.

`serialize_response(status, cseq, headers, body)` uses a finite standard status
list, generates CSeq and exact Content-Length, and applies matching output bounds.
Callers cannot override framing or inject CRLF in headers. It does not generate
successful protocol exchanges automatically. Read/write deadlines and response
backpressure remain the managed transport's responsibility.

## SDP ANNOUNCE

`sdp::parse_announce(body)` returns typed `Announce` requirements. It accepts the
current Moonlight Gen7 shape: `v/o/s`, vendor attributes, `t=0 0`, and the shortened
`m=video <port>` line. Advertised origin and port are metadata, never authority to
send traffic to a client-selected address. The 16 KiB document, 192 lines, 1024
bytes per line and 128 attributes are independently bounded. Duplicate fields or
attributes, unsigned-number overflow, missing required parameters and malformed
CRLF input fail explicitly. Unknown extension attributes outside recognized
media/security families may be ignored within these bounds; unknown parameters
inside those families fail rather than accidentally enabling a new media mode.

The default profile is H.264, SDR, 4:2:0, one slice, even dimensions
16–3840 by 16–2160, and integer 1–120 fps. Color modes are BT.601 or BT.709,
limited or full range. A supplied nonzero refresh rate must agree with FPS within
0.5 Hz. Reference frame requests are zero (host choice) or one. Encoder minimum,
maximum, initial and peak bitrates must agree and be 100–100,000 Kbps. The optional
Moonlight configured bitrate is preserved separately and must not be smaller.
Packet size is 256–1392 bytes, divisible by 16, excluding the 16-byte RTP header.
Audio is stereo, channel mask 3, ordinary quality, with 5 or 10 ms packets.

HEVC Main SDR bitstream format 1 is parsed only when the client explicitly
indicates HEVC support. It remains rejected by default support and by the
current RTSP capability until an encoder and session can actually supply it.
AV1, HDR, 4:4:4, multiple slices, surround audio, dynamic resolution,
intra refresh, recovery mode, BLL-FEC, variable bitrate negotiation and unknown
critical feature bits fail explicitly. Gen3 binary attributes, Gen4 defaults,
fractional FPS strings and generic RFC SDP media profiles are not supported.
These are protocol-profile restrictions, not inferred encoder capabilities.

**Parsing does not accept the transport requirements.** Call
`Announce::check_support(&TransportSupport)` with real backend capabilities before
committing ANNOUNCE. The default support value denies all transport features.
The result retains ENet control mode, encrypted control v2, legacy remote-input
and audio encryption flags, video/audio encryption, and FEC minimum/percentage.
Encrypted control covers remote-input encryption; standalone legacy input
encryption requires separate support. Unknown encryption bits are rejected.
FEC enable must be one because it selects the GameStream sequencing format;
a positive minimum, positive repair percentage or absent host-selected percentage
additionally requires parity support. An omitted percentage remains `None`, so
a host must choose its policy explicitly rather than infer zero parity.
The fixture's ordinary minimum of two must never be silently changed to zero.
Neither this check nor parsing implements encryption, control or audio transport.
The consumer must also intersect negotiated media limits with real encoder,
capture, audio and packetizer capabilities and its host-required encryption policy.

## Video packetization and version profile

See [the video module's API, tests and remaining gates](src/video/README.md).
The stream owner must choose one advertised host version consistent with
RTSP negotiation, control framing, SDP and video frame headers. In particular,
the short video header targets Gen7 eight-byte variants and cannot be advertised
as the historical 7.1.320–7.1.349 twelve-byte variant. The current management
server's advertised version is not a completed streaming capability profile.
No advertised versions or readiness responses are changed by this library.

## Audio packetization and compatibility

[AudioPacketizer](src/audio/README.md) emits fixed-duration 48 kHz stereo Opus RTP,
with four data shards and two parity shards using the client's fixed audio matrix.
Its timestamps advance in milliseconds. Constant wire payload length is required;
legacy CBC encryption runs before parity generation. Discontinuity handling drops
an incomplete parity group and reports skipped sequence positions explicitly.
The coordinator must synchronize the sealer and account for larger clock gaps.

Moonlight enables this audio FEC queue only for advertised host versions at least
7.1.415; multi-block video requires at least 7.1.431 and its explicit packetizer
block limit. These restrictions must be combined with the short-frame-header
profile above. The current HTTP adapter's 7.1.0.0 advertisement does not establish
that these recovery modes are available. Upstream-C parity/recovery oracles pass,
but they are not complete-client playback or audio/video synchronization tests.

## Media crypto boundary

[VideoSealer and AudioSealer](src/crypto/README.md) implement host-to-client video
AES-128-GCM framing and legacy audio AES-128-CBC with PKCS#7 padding. Video adds a
32-byte prefix after RTP/FEC packetization; audio encrypts only Opus before audio
FEC. Reserve all queue space before mutating packetizer or cipher state.

A video sealer owns a non-repeating invocation counter and stops at exhaustion.
The owner must prevent recreation with the same launch key, including across
restarts. Video's outer frame hint is unauthenticated by the client protocol;
authenticated RTP/NV data remains inside the ciphertext. Legacy audio CBC supplies
no authentication or replay protection, and its sequence-derived IV repeats after
65,536 positions. The caller explicitly chooses to stop before reuse or permit
legacy wrap; a discontinuity does not reset that budget. Modern control and encrypted RTSP envelopes are also implemented with separate
directional nonce domains and replay tracking. Key exchange and peer/session
authorization remain outside these helpers.

## Reliable control framing

The [ENet codec](src/enet/README.md) and [peer engine](src/enet/peer/README.md)
implement bounded uncompressed, checksum-free control connections in Rust.
Primary C fixtures verify wire fields, and an original upstream C client checks
handshake, reliable delivery, fragmentation and disconnect. Control AES-GCM
uses a shared replay window across channels; encrypted RTSP has its own bounded
framing decoder and directional counters. The
[crypto contract](src/crypto/README.md) documents supported profiles and limits.
These pieces do not issue launch authorization or advertise streaming readiness.

## Evidence and validation

Primary sources were inspected on 2026-09-13, including current upstream and
moonlight-common-c commit `874ac9548f1bd6f095ef2b435c42cdde460e7821`:

- [RtspConnection.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/RtspConnection.c): client methods, targets and headers.
- [SdpGenerator.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/SdpGenerator.c): ANNOUNCE attribute names, units and wire shape.
- [Rtsp.h](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/Rtsp.h): RTSP message representation.
- [Limelight-internal.h](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/Limelight-internal.h): encryption and client feature bits.
- [RFC 2326](https://www.rfc-editor.org/rfc/rfc2326.html), sections 4, 6 and 12: request syntax, CSeq and Content-Length framing.

Sibling Vibeshine `src/rtsp.cpp` was checked at the parity baseline
`aa292c4cb097f22beab19d6ebf0307367f9bf449`. The working checkout
`d9722f8e6c75e1b84908d2ddf6997684787dce82` was also inspected as an additional
behavior reference, not a replacement baseline; that file is unchanged between
these commits. No source was copied or translated. The checked-in SDP fixture is independently
constructed from client field names and representative values, not a captured
successful session. Tests exercise every fragmentation boundary of a complete
ANNOUNCE, coalesced requests, bytewise input, truncation, duplicate/framing attacks,
serializer injection, typed requirements, unsupported modes and bounded inputs.
Real Moonlight RTSP handshakes, session ownership, control/input/audio channels,
network impairment and Windows end-to-end playback remain integration gates.
