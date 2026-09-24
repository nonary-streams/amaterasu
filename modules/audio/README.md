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
{"action":"open","endpoint":"","virtual_sink":"","channels":6,"operation_id":"surround-audio"}
{"action":"lookup","operation_id":"living-room-audio"}
{"action":"status"}
{"action":"status","stream":1}
{"action":"close","stream":1}
```

Enumeration returns `{ok:true,enabled,endpoints:[{id,name,description,adapter_name,default_multimedia}]}`
for active render endpoints. Explicit endpoint IDs come from that list; `default`
resolves the current multimedia render endpoint once when opened. The selected
endpoint remains fixed for the session. Enumeration is limited to 64 endpoints
and a 12,000-byte metadata reply. On Windows, opening an explicit endpoint with
`capture_sink_without_routing` omitted or false temporarily assigns that endpoint
as the Console, Multimedia, and Communications render default, unless it is
already the Console default. Concurrent captures of the same endpoint share the
assignment; a conflicting endpoint open fails. When the final capture closes,
the provider restores only roles it assigned that still point to the selected
endpoint, preserving role changes made elsewhere. If assignment fails, open
fails and attempts to restore roles changed earlier in the same open. A failed
restore blocks later routed opens, which retry cleanup first; there is no
background retry. Opening `default`
does not change default roles. The provider does not change volume or mute state.

An open with `virtual_sink` selects a managed virtual render endpoint before the
regular `endpoint`. `virtual_sink:null` or omission keeps regular endpoint
selection. An empty string matches the active endpoint whose adapter name is
exactly `Steam Streaming Speakers`; nonempty text matches an active endpoint's
ID, friendly name, device description, or adapter name, in that priority order.
Matching is exact and case-sensitive. The virtual selector never falls back to
the regular endpoint or the default endpoint when it is unavailable. `channels`
selects stereo (2), 5.1 (6), or 7.1 (8); when omitted in virtual mode it defaults
to stereo. The backend reads the current Console render mix's valid bit depth
(using 32 bits if it cannot query the mix) and tries only matching virtual
formats in Vibeshine's order. Stereo tries signed 24-in-32, signed 24, signed
16, float32, then signed 32. 5.1 and 7.1 try float32, signed 32, signed
24-in-32, signed 24, then signed 16; 5.1 tests back-speaker and side-speaker
masks for each format. Unsupported preferred depths or Windows rejecting every
candidate return `virtual_sink_format_unavailable`. Loopback capture requests
48 kHz float32 and uses WASAPI shared-mode PCM conversion for the configured
layout, while the selected endpoint's engine format change persists after
capture closes. Capture-only routing is invalid for virtual selection; virtual
opens use the normal shared default-role lease and cleanup.

Set `capture_sink_without_routing:true` on an open to capture the selected
endpoint without changing any render default role. The product resolver should
set it only for Vibeshine's capture-only case: capture-only enabled, a configured
sink present, no configured virtual sink, and the selected sink equal to that
configured sink. The audio module receives this per-open policy snapshot; it does
not derive eligibility from product settings.

Open/lookup return `{ok:true,stream,endpoint,requested_endpoint,virtual_sink,channels,capture_sink_without_routing,format}`.
`endpoint` is the selected endpoint ID, `requested_endpoint` is the caller's
regular selector, `virtual_sink` is the requested virtual selector, and `channels`
is the resolved requested virtual layout. An omitted policy field is accepted
as false for older callers. Format contains
`sample_rate`, `channels`, `channel_mask`, and `format:"f32"|"i16"`. Status returns
`{ok:true,enabled,audio_route_restore_pending,streams:[...]}` with only the
caller's streams, optionally filtered by ID. The route flag reports a failed
final restoration attempt until a later routed open retries cleanup. Each stream
includes its selected/requested endpoint, format,
`state:"capturing"|"failed"`, `failure`, `discontinuities`, and `dropped_packets`.
Close returns `{ok:true}`. Missing/foreign lookup or close returns
`{ok:false,code:"stream_not_found"}`; absent status returns an empty list.
Driver error strings and PCM samples are never placed in control replies.

The optional operation ID is 1–128 ASCII graphic bytes. Same-caller retry with
that ID, requested endpoint, virtual selector, layout, and routing policy reuses
the original stream; changing any of those values conflicts. After close, retry returns
`stream_closed` and lookup returns `stream_not_found`. Up to 128 operation
records remain in memory per module instance. There are at most four live
streams, two per caller. Open
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

The Windows backend uses official `windows` crate COM bindings. Explicit routed
opens use the undocumented Windows `IPolicyConfig::SetDefaultEndpoint` COM
interface at a documented ABI boundary; virtual opens also use its
`SetDeviceFormat` method at that boundary. Inability to snapshot or switch roles
fails the capture open. Each stream has
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
selected-endpoint capture-only case is represented by the per-open policy above.
Opus encoding, configured speaker remapping, resampling, continuous silent audio,
process-specific loopback, microphone capture, Linux/macOS backends, automatic
endpoint switching, and full streaming audio parity remain unimplemented.

Host tests use fake endpoint/capture backends and real native callback bindings
for selection, ownership, idempotency, cancellation rollback, silent packets,
discontinuities, timestamps, queue overflow, sink retry, and close/drop. Pure
route-policy tests cover same-target leases, conflicting targets, partial
assignment rollback, and preserving roles changed elsewhere. They do not call
the Windows role-switch COM interface or capture desktop audio. Windows and
Linux target checks validate compilation; a Windows-only mix-format test
additionally checks extensible channel layout. The ignored Windows capture-only
hardware test requires controlled test audio on a selected render endpoint and
checks all three default render roles before and after:

```text
cargo test -p amaterasu-audio explicit_endpoint_captures_without_changing_any_default_role -- --ignored
```

That test has not been run on development hardware. Real Windows qualification
of routed opens must exercise successful switching/restoration, each partial
switch failure and rollback, role changes made by another application, device
removal, and recovery after restoration failure. Waveform/channel correctness,
silence, sustained backpressure, discontinuities, and cancellation while drivers
initialize also need hardware coverage.

Primary API references:

- [Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- [Capturing a stream](https://learn.microsoft.com/en-us/windows/win32/coreaudio/capturing-a-stream)
- [GetMixFormat](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat)
- [IAudioCaptureClient GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
