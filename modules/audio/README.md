# WASAPI loopback audio

`amaterasu-audio` provides `amaterasu.audio/1` control and the matching native
`amaterasu_media::AudioCaptureApi`. Startup settings default to
`{"enabled":false}`. Disabled enumeration returns an empty list without starting
COM or touching audio endpoints; open returns `disabled`. On non-Windows systems,
enabled enumeration is empty and open returns `unsupported_platform`.

Strict JSON controls:

```json
{"action":"enumerate"}
{"action":"open","endpoint":"default","operation_id":"living-room-audio"}
{"action":"lookup","operation_id":"living-room-audio"}
{"action":"status"}
{"action":"status","stream":1}
{"action":"close","stream":1}
```

Enumeration returns `{ok:true,enabled,endpoints:[{id,name,default_multimedia}]}`
for active render endpoints. Explicit endpoint IDs come from that list; `default`
resolves the current multimedia render endpoint once when opened. The selected
endpoint remains fixed for the session. Enumeration is limited to 64 endpoints
and a 12,000-byte metadata reply. The provider never changes any endpoint default
role, volume, mute state, visibility, or device configuration.

Open/lookup return `{ok:true,stream,endpoint,format}`. Format contains
`sample_rate`, `channels`, `channel_mask`, and `format:"f32"|"i16"`. Status returns
`{ok:true,enabled,streams:[...]}` with only the caller's streams, optionally
filtered by ID. Each stream includes its selected/requested endpoint, format,
`state:"capturing"|"failed"`, `failure`, `discontinuities`, and `dropped_packets`.
Close returns `{ok:true}`. Missing/foreign lookup or close returns
`{ok:false,code:"stream_not_found"}`; absent status returns an empty list.
Driver error strings and PCM samples are never placed in control replies.

The optional operation ID is 1–128 ASCII graphic bytes. Same-caller retry with
that ID and the same requested endpoint reuses the original stream; selecting a
different endpoint conflicts. After close, retry returns `stream_closed` and
lookup returns `stream_not_found`. Up to 128 operation records remain in memory
per module instance. There are at most four live streams, two per caller. Open
checks request liveness before dispatch and closes a newly created stream if its
completion cannot be delivered. A cancelled lookup or retry does not close an
existing stream. Callers reconciling uncertain opens must settle their original
runtime request before using owned lookup and close.

Native bindings copy the authenticated endpoint principal. `read_audio` rejects
another caller's stream IDs. Each successful callback borrows one interleaved PCM
packet, valid only until return; the consumer must copy retained samples. Sink
rejection retains the packet for retry. Concurrent or reentrant calls return
`MEDIA_BUSY`; no packet returns `MEDIA_PENDING`; missing/closed returns
`MEDIA_CLOSED`; device/worker failure returns `MEDIA_BACKEND_ERROR` and requires
close/reopen. A provide/call grant for `amaterasu.audio/1` is required at the
respective endpoint. These are trusted native modules, not a native-code sandbox.

The Windows backend uses official `windows` crate COM bindings. Each stream has
a dedicated apartment thread that activates `IAudioClient`, validates its mix
format, initializes shared-mode `AUDCLNT_STREAMFLAGS_LOOPBACK`, and starts
`IAudioCaptureClient`. It copies complete WASAPI packets before releasing their
borrowed buffers on that same thread. Polling sleeps two milliseconds only when
no packet is available. No COM pointer or borrowed WASAPI allocation crosses a
thread boundary. Endpoint enumeration uses a short-lived separate apartment.

Supported formats are little-endian 32-bit IEEE floating point or 16-bit signed PCM,
8–192 kHz and 1–8 channels. There is no format conversion or resampling.
WAVEFORMATEXTENSIBLE valid bits, block alignment, byte rate, subtype and channel
mask are checked. `channel_mask` preserves the platform speaker mask; interleaved
samples follow ascending speaker-mask bit order. Zero means unspecified layout.
Legacy mono/stereo imply front center/front left-right respectively. Consumers
must use this metadata before interpreting surround channels; channel count
alone is insufficient. Unsupported 24/32-bit integer mixes fail open explicitly.
Packets longer than 100 ms or larger than 1 MiB fail with
`unsupported_packet_size`; they are not truncated.

`AUDIO_SILENT` produces an initialized zero-filled PCM packet without reading the
WASAPI silent buffer pointer. `AUDIO_DISCONTINUITY` reports engine glitches,
including an initial discontinuity if WASAPI marks it. It increments the status
counter and does not automatically terminate capture. No packets are synthesized
when a silent endpoint produces no WASAPI packets. Normal timestamps are QPC
100ns positions normalized to the first valid packet and converted to relative
microseconds. Timestamp-error packets use elapsed monotonic stream time with
`AUDIO_TIMESTAMP_ESTIMATED`; timestamp regression is clamped and marked estimated.
Sequence numbers increase per packet. This is not a complete audiovisual clock
synchronization solution.

The worker queue holds sixteen packets, plus at most one retained callback
packet. A full queue stops capture and records `audio_queue_overflow` with a
nonzero dropped-packet counter. It does not silently overwrite old audio. Native
reads are nonblocking queue reads; close signals the worker and joins it before
releasing module-owned code. Driver activation, GetBuffer, Stop or COM teardown
can still block indefinitely. A hung driver therefore needs external host
supervision; no hard teardown deadline or crash recovery is claimed.

Vibeshine `src/audio.h` and the Windows audio policy/capture behavior were used as
behavioral references without copying or translating source. The pinned
selected-endpoint regression requires capturing the chosen endpoint without
changing defaults; this implementation has no default-role mutation API.
Opus encoding, configured speaker remapping, resampling, continuous silent audio,
process-specific loopback, microphone capture, Linux/macOS backends, automatic
endpoint switching, default-role mutation/recovery, and full streaming audio
parity remain unimplemented.

Host tests use fake endpoint/capture backends and real native callback bindings
for selection, ownership, idempotency, cancellation rollback, silent packets,
discontinuities, timestamps, queue overflow, sink retry, and close/drop. They do
not capture desktop audio. Windows and Linux target checks validate compilation;
a Windows-only mix-format test additionally checks extensible channel layout.
The ignored Windows hardware test requires controlled test audio on a selected
render endpoint and checks all three default render roles before and after:

```text
cargo test -p amaterasu-audio explicit_endpoint_captures_without_changing_any_default_role -- --ignored
```

That test has not been run on the development Mac. Real Windows qualification
must also check waveform/channel correctness, device removal, silence, sustained
backpressure, discontinuities, and cancellation while drivers initialize.

Primary API references:

- [Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- [Capturing a stream](https://learn.microsoft.com/en-us/windows/win32/coreaudio/capturing-a-stream)
- [GetMixFormat](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat)
- [IAudioCaptureClient GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
