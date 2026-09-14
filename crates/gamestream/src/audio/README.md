# GameStream Opus audio packetization

`AudioPacketizer` emits the GameStream audio wire format for 48 kHz stereo with
5 ms (240 decoded frames) or 10 ms (480 frames) Opus packets. It performs no audio
capture, encoding, network delivery, session negotiation, or key management.

`AudioConfig` requires sample rate, channel count, frames per packet, constant
`wire_payload_bytes`, and SSRC (normally zero). `packetize` accepts plaintext Opus;
`packetize_sealed` invokes a supplied legacy CBC sealer before computing parity.
Both accept the known decoded-frame count and validate it against the negotiated
duration and the Opus TOC/framing. Framing checks follow RFC 6716; they do not
entropy-decode samples or establish perceptual audio correctness. Mono-coded Opus
frames within a negotiated stereo stream remain valid codec behavior.

The data RTP header is 12 bytes: version 2, payload type 97, big-endian sequence,
millisecond timestamp and SSRC. The clock advances by 5 or 10 **milliseconds**, not
by 240 or 480 samples. Sequence and timestamp wrap as unsigned 16/32-bit values.
Data groups start at sequence multiples of four. Every push emits one data
packet immediately; the fourth also emits two parity packets. Parity RTP payload
type is 127, its outer timestamp is zero, and its sequence values overlap the next
two data sequence values without consuming the data counter. Its additional
12-byte FEC header contains shard index, data payload type, and big-endian group
base sequence, base timestamp, and SSRC.

Audio uses the receiver's fixed two-by-four OpenFEC matrix, not video's Cauchy
matrix. The implementation reuses the independently verified GF(256) arithmetic
with the audio protocol coefficients. Parity covers only the four equally sized
wire payloads, excluding RTP headers. This includes ciphertext when encryption
is enabled. A lost data RTP header is reconstructed by the receiver from the FEC
header and negotiated packet duration.

## Lengths, encryption, and lifecycle

The receiver rejects different shard lengths within a group and carries no
original-length field for arbitrary padding. The packetizer therefore enforces
one constant wire length for the stream; it does not zero-pad Opus or ciphertext.
Use a CBR encoder profile. Plaintext input is bounded to 1275 bytes; legacy CBC
with PKCS7 produces at most 1280 bytes. Configured wire payloads cannot exceed
1376 bytes, preserving the client's 1400-byte receive limit including FEC headers.
There is no promise to support arbitrary VBR output simply because the codec can
produce it. CBC can preserve different original lengths only when their padded
ciphertext lengths stay equal.

`packetize_sealed` validates input and expected PKCS7 size before invoking its
callback. Integrate it with `crypto::AudioSealer::encrypt(sequence, opus)` and map
failure to `AudioError::EncryptionFailed`. No cipher keys are owned by this module.
The surrounding stream must terminate on callback failure or an unexpected
ciphertext length, since the callback may have consumed cryptographic state even
though the packetizer did not commit. Never recreate a sealer with the same key
to roll back a failed send. The crypto module documents legacy CBC's lack of
authentication, explicit sequence-wrap policy, and IV exhaustion handling.

`required_datagrams()` returns 1 or 3; `maximum_datagram_bytes()` provides a safe
per-datagram queue reservation. Reserve capacity before any encryption or
packetization. `validate_input()` is non-mutating. At most three prior data
payloads remain between calls; invalid input changes no sequence, timestamp, or
pending group. There are no detached workers or unbounded queues in this library.

`reset_discontinuity()` discards cached parity inputs, skips to the next sequence
multiple of four, and advances the packet clock for skipped positions. Its return
value reports discarded shards, skipped packets, next sequence and timestamp.
Already emitted data is not recalled and no parity is fabricated for a partial
group. Synchronize an encrypted stream with `AudioSealer::skip_to(next_sequence)`
when positions were skipped. This reset does not establish elapsed wall-clock
capture time; the coordinator owns larger timing discontinuities. Dropping the
packetizer closes it without emitting an incomplete group.

## Verification and remaining gates

The primary receiver reference is moonlight-common-c
`874ac9548f1bd6f095ef2b435c42cdde460e7821`:

- [RtpAudioQueue.h](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/RtpAudioQueue.h): four-data/two-parity grouping and FEC header.
- [RtpAudioQueue.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/RtpAudioQueue.c): fixed matrix, equal lengths, group alignment and recovery.
- [AudioStream.c](https://github.com/moonlight-stream/moonlight-common-c/blob/874ac9548f1bd6f095ef2b435c42cdde460e7821/src/AudioStream.c): receive bound, millisecond clock and decryption after recovery.
- [RFC 6716 section 3](https://www.rfc-editor.org/rfc/rfc6716.html#section-3): Opus TOC and packet framing.

The sibling was inspected for behavior without copying or translating source.
Regular tests include explicit wire/parity fixtures, independent field parsing,
5/10 ms clocks, wrapping, partial groups, constant-length rejection, malformed
Opus, encryption order, and error atomicity. Two explicit upstream-C tests compare
parity and recover every pair of erasures across six shards at multiple lengths,
and recover actual AES-CBC ciphertext then decrypt it to the original Opus after
a discontinuity. Both passed against the pinned nanors decoder with the exact
matrix installed by Moonlight's audio queue.

Reproduce using the same external oracle as video (no upstream source is embedded):

```sh
crates/gamestream/src/video/fec/build-oracle.sh /path/to/nanors /tmp/amaterasu-nanors-oracle
AMATERASU_NANORS_ORACLE=/tmp/amaterasu-nanors-oracle cargo test -p amaterasu-gamestream -- --include-ignored
```

The helper verifies nanors commit `b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a`
and a clean tracked tree; checksums are in `../video/README.md`. Oracle tests are
ignored by default and require the explicitly built C executable.

Moonlight disables this audio FEC queue for server versions below 7.1.415. The
current HTTP adapter advertises 7.1.0.0, so a coordinator must resolve that version
profile before claiming this recovery path in end-to-end streaming. Execution of
the complete client queue, actual Moonlight playback, pacing, audio/video sync,
and loss/jitter behavior remain qualification gates. This packetizer does not
implement surround, arbitrary packet durations, or control-channel negotiation.
