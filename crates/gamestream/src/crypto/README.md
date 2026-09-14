# GameStream media encryption

This module implements host-to-client media, modern Sunshine control, and encrypted
RTSP envelopes. It does not negotiate capabilities, authorize peers, manage sockets
or parse keys from user requests. Select the matching profile and direction only
after allocating a session with a fresh 128-bit launch key.

## Video

```rust,ignore
let mut video = VideoSealer::new(&launch_key, VideoProfile::SunshineHostToClient)?;
let encrypted_datagram = video.seal(frame_index, &plaintext_rtp_nv_packet)?;
```

The complete UDP payload is a 32-byte prefix followed by AES-128-GCM ciphertext:
12 nonce bytes, a little-endian 32-bit frame hint, a 16-byte tag, then ciphertext.
The plaintext is the complete RTP/NV packet, including video FEC data or parity.
Seal each datagram **after** video packetization and FEC generation. The prefix
adds 32 bytes and must be included when negotiating packet size and reserving
bounded output queues. Plaintext must contain 16–65,475 bytes; the final UDP
payload never exceeds 65,507 bytes. The packetizer and path MTU can impose lower
limits. This API checks size, not validity of a caller's RTP/NV packet.

The nonce uses a little-endian 64-bit invocation counter, three zero bytes and
`V`. It begins at zero, advances once per cryptographic attempt, permits the
last counter value once, then permanently rejects further packets. There is no
reset, clone, supplied nonce or public counter-restore API. A failed send must
not rewind the sealer. Size rejection consumes no counter; encryption failure
burns its counter. Callers must reserve queue capacity before sealing so a BUSY
result does not silently consume caller media or advance cryptographic state.

The client uses empty GCM AAD. Therefore the outer frame hint is deliberately
**not authenticated**; do not use it as an authorization or replay decision.
Nonce, ciphertext and tag tampering fail authentication. Authenticated RTP/NV
metadata is inside the ciphertext. This module does not claim that encryption
alone supplies replay protection; a receiver still needs its sequence/frame
policy. The host-to-client video nonce domain is distinct from the `HC` and
`CC` domains used by the newer control protocol below.

## Legacy audio

```rust,ignore
let mut audio = AudioSealer::new(&launch_key, key_id, AudioIvPolicy::RejectReuse);
let encrypted_opus = audio.encrypt(rtp_sequence, &opus_packet)?;
```

Audio uses AES-128-CBC with PKCS#7 padding, encrypting only the Opus payload before
fixed-block audio FEC. RTP headers remain outside encryption. Its 16-byte IV is
`BE32(key_id + u16_sequence)` followed by twelve zeros; addition wraps modulo
2^32 exactly as in the client. There is no extended sequence number in this IV.
Plaintext is 1–1275 bytes; ciphertext adds 1–16 bytes, at most 1280 total. The
packetizer must enforce its negotiated constant ciphertext size before calling
`encrypt`; padding must not be used as an excuse to truncate variable packets.

The sealer accepts the first RTP sequence, then requires the next wrapping u16
sequence. `skip_to(next_sequence)` explicitly reserves missing positions during
a discontinuity, allowing forward gaps of at most 32767. Skipped positions count
toward the IV budget. A backwards/ambiguous jump fails, and no method resets the
key or budget. Coordinate packetizer discontinuities with this method; a partial
FEC block must not be joined to later unrelated samples.

Choose one policy explicitly:

- `RejectReuse` permits at most 65,536 distinct sequence positions, including
  skipped positions, then returns `AudioIvExhausted`. End or rekey the session
  before this point: at continuous 5 ms packets this is 327.68 seconds; at 10 ms,
  655.36 seconds. A numerical u16 wrap from a nonzero starting sequence is allowed
  while all derived IVs are still distinct.
- `PermitLegacyWrap` preserves the legacy protocol across repeated u16 cycles.
  Its IVs repeat with the same key after 65,536 positions;
  `iv_reuse_started()` reports this after the first repeated position is consumed.
  This is an explicit compatibility concession, not improved security.

CBC provides **no authentication or replay protection**, under either policy.
Valid padding does not establish authenticity: tests demonstrate modified
ciphertext that still decrypts with valid padding. There is no public CBC
receiver/decryption API or padding-error endpoint in this module.

## Modern control envelopes

`ControlSealer::new(key, ControlProfile::SunshineV2, direction)` owns one sending
counter, starting at zero. `seal(kind, payload)` encrypts the little-endian inner
16-bit kind and payload length plus the payload. The wire envelope is little-endian
type 1, 16-bit length (everything after the first four bytes), 32-bit sequence,
16-byte GCM tag, then ciphertext. Payloads up to 65,511 bytes fit the complete
65,539-byte envelope. The ENet layer owns fragmentation; this is not one raw UDP
datagram. There is no implicit fallback: `ControlProfile::NvidiaLegacy` returns
`UnsupportedProfile`, rejecting Nvidia's old 16-byte/8-bit IV construction.

`ControlOpener::new(key, profile, direction, ordering)` verifies exact outer and
inner lengths and returns `ControlMessage {sequence, kind, payload}`. The caller
must still decode the control kind and authorize its effect. `Direction` selects
`HostToClient` or `ClientToHost`, binding the 12-byte nonce to `HC` or `CC`:
little-endian sequence in bytes 0–3, zeros in 4–9, direction in 10 and `C` in 11.
This domain also prevents authentic messages from being reflected back in the
opposite direction. The protocol uses empty AAD.

`SingleReliableChannel` requires strictly increasing accepted sequences, allowing
gaps. It is appropriate only when the caller uses one globally ordered reliable
channel. `MultipleChannels` keeps a fixed 65,536-bit replay window (8 KiB), shared
across all ENet channels for the key/direction. Each sequence can be accepted once;
authentic older packets beyond the window are rejected. At 6400 messages/second
this represents roughly ten seconds of reordering, not a general guarantee for
arbitrarily delayed reliable retransmissions. This bound is an intentional
receiver policy; reliability and resynchronization beyond it belong to the
managed control protocol. Do not create one opener per channel.

## Encrypted RTSP envelopes

`RtspSealer::new(key, direction)` starts at sequence one. `seal(plaintext)` wraps
a complete serialized request or response. The 24-byte prefix contains big-endian
`0x80000000 | ciphertext_length`, a big-endian 32-bit sequence and the 16-byte tag.
The nonce nevertheless uses the sequence in little-endian, six zeros, then `HR`
(host response) or `CR` (client request). Nonempty plaintext is capped at 80 KiB,
matching the RTSP parser's header-plus-body bound. Empty AAD matches the client.

`encrypted_rtsp_frame_length(prefix)` returns `None` until four bytes are present,
then validates the encrypted marker and bounded declared length. It authenticates
nothing. `RtspEnvelopeDecoder` provides connection-local incremental `push`,
`next_frame` and `finish` framing with at most two maximum-size envelopes buffered.
Drain between reads. Framing errors poison the decoder; EOF with residual bytes
is an error. Neither API falls back to cleartext RTSP when encryption is selected.

`RtspOpener::new(key, direction)` verifies exactly one envelope, rejects trailing
or partial bytes, and returns `RtspMessage {sequence, plaintext}`. Pass plaintext
to the actual RTSP request/response parser. One session-wide 128-sequence replay
window permits independent TCP transactions to arrive out of order. Keep both
sealer and opener across connections; constructing them per connection would
reuse sending nonces and forget accepted requests. Authentication success consumes
the RTSP sequence even if later RTSP parsing or application authorization fails.
This prevents retrying a previously authenticated mutation after an error.

Both control and RTSP use private non-cloneable counters. The last u32 value is
used once, then sending permanently returns `NonceExhausted`; no wrap or reset is
provided. Reordered receive sequences are ordinary unsigned values, never a new
counter epoch. Failed authentication, malformed outer framing, or invalid control
inner lengths leave accepted replay state unchanged. Peer/session failures must
never be recovered by recreating these objects with an old launch key. Openers
are not Debug/Clone and never expose key material. No new dependencies were added
for these envelopes; all GCM operations use ring.

## Key ownership and validation

Sealers own expanded keys and are neither Clone nor Debug. Constructors borrow a
fixed 16-byte key; the caller owns its original key storage and must avoid logging
or retaining unnecessary copies. The AES crate's zeroize feature clears stored
CBC key schedules on drop. No promise is made about compiler-created copies or
ring's expanded-key erasure. The returned errors contain only static categories.

The owner must reserve each launch key for one session across all recreation
attempts and prevent a new video sealer from starting at zero with that key.
A bounded private key history can prevent reuse during one process lifetime;
process restart still requires fresh launch keys or durable anti-reuse state.
Sharing the session launch key across the documented video/audio/control domains
is the protocol's design; do not reuse it for an independent video session.
This is trusted native code, not a key-isolation boundary or native-code sandbox.

Dependencies are the already-used ring 0.17.14 and AES 0.8.4 implementations, plus
RustCrypto CBC 0.1.2 and its block-padding dependency. No cipher, authentication,
block mode or padding algorithm is implemented locally. The CBC mode is selected
only for wire compatibility; new protocols should use authenticated encryption.

## Evidence and tests

The primary client was inspected at moonlight-common-c
`874ac9548f1bd6f095ef2b435c42cdde460e7821`:

- [Video.h](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/Video.h) and [VideoStream.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/VideoStream.c): prefix offsets, key size and video decryption.
- [AudioStream.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/AudioStream.c): sequence-derived IV, CBC reset and final padding.
- [ControlStream.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/ControlStream.c): separate control direction domains.

Pinned sibling Vibeshine `aa292c4cb097f22beab19d6ebf0307367f9bf449`
`src/stream.cpp` supplies the host video counter/domain convention and confirms
audio encryption before FEC. Its source was inspected only as a behavioral
reference; no source was copied or translated.

`vectors.txt` contains fixed **public fixture keys**, not credentials. The
independent generator uses Python cryptography's AESGCM and AES/CBC implementations
and standard PKCS#7 padding. It covers video nonces 0/1, prefix endian layout,
audio nonzero sequence/key ID, 32-bit IV addition wrap, and full-block padding.
Run `python3 crates/gamestream/src/crypto/generate_vectors.py --check` to reproduce
all five vectors (verified with cryptography 48.0.1). Rust tests also exercise
tampered video, unauthenticated frame hints, exhaustion, discontinuities, payload
bounds, and both audio wrap policies. These are primitive/wire-layout checks,
not successful playback through a real Moonlight client or loss-impaired network.

Control and RTSP wire evidence additionally comes from the pinned primary
`ControlStream.c` and `RtspConnection.c`, and pinned sibling `src/stream.cpp` and
`src/rtsp.cpp` via `git show` at the baseline above. `envelope_vectors.txt` contains
eight independent Python cryptography fixtures covering all four domains and
nontrivial u32 sequence endianness. The same generator's `--check` verifies them.
Envelope tests cover reflected directions, tag/ciphertext/sequence tampering,
failed-authentication state preservation, authenticated malformed inner framing,
ordered and reordered replay policies, window expiration, invocation exhaustion,
maximum lengths, fragmented/coalesced RTSP and truncated EOF. They do not implement
an ENet session, validate a launch ticket or prove a complete encrypted handshake.
