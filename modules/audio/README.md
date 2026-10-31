# Desktop loopback audio

`amaterasu-audio` provides `amaterasu.audio/1` control and the matching native
`amaterasu_media::AudioCaptureApi`. Startup settings default to
`{"enabled":false}`. Disabled enumeration returns an empty list without starting
COM or connecting to an audio server; open returns `disabled`. Windows uses
WASAPI loopback. Linux uses monitor sources from the user's existing
PulseAudio-compatible server, including PipeWire's Pulse server. Other operating
systems return an empty endpoint list and `unsupported_platform` on open.

The native service registers the `amaterasu.audio-capture.native/1` interface
family through `amaterasu_module_sdk::audio::register_capture`. Alternative
providers use the same SDK adapter and PCM contract. Consumers bind with
`bind_capture`, which requires the runtime to verify that family before exposing
the table. Trusted startup composition and signed grants still choose and
authorize the service; native providers are trusted code. C providers use
`AMATERASU_AUDIO_CAPTURE_CONTRACT` from `amaterasu_media.h` with the runtime's
`native_register_typed` operation; C consumers use the same identity with
`native_bind_typed`. A provider registered only with the legacy untyped operation
cannot serve the typed stream consumer.

Strict JSON controls:

```json
{"action":"enumerate"}
{"action":"open","endpoint":"default","host_audio_enabled":false,"operation_id":"living-room-audio"}
{"action":"open","endpoint":"default","capture_format":{"sample_rate":48000,"channels":6,"sample_format":"f32"}}
{"action":"open","endpoint":"default","operation_id":"continuous-audio","continuous_audio":{"packet_ms":5}}
{"action":"open","endpoint":"","virtual_sink":"","channels":6,"operation_id":"surround-audio"}
{"action":"lookup","operation_id":"living-room-audio"}
{"action":"status"}
{"action":"status","stream":1}
{"action":"close","stream":1}
```

The NVHTTP product setting `install_steam_audio_drivers` defaults to `true` and
is global-only. NVHTTP sends the loaded setting to the audio provider after
configuration becomes available; a provider outage is retried without blocking
NVHTTP startup or writes. On Windows, the provider checks for an active
`Steam Streaming Speakers` render endpoint and, if absent, best-effort calls
`DiInstallDriverW` from System32 for
`%CommonProgramFiles(x86)%\\Steam\\drivers\\Windows10\\x64\\SteamStreamingSpeakers.inf`.
This may require administrator access. The installer runs on a separate STA
worker and polls for endpoint activation every 500 ms for up to 30 seconds. It
restores Console, Multimedia, and Communications defaults only when each role
still points at the newly installed Steam endpoint. Endpoint identity uses the
registered Valve driver provider and Steam device/driver description metadata,
not the user-editable endpoint name alone. If activation times out, it applies
the same metadata check to each current default. Before every restore it reads
the role endpoint ID again and skips the write if that ID changed after the
identity check. Windows does not offer an atomic compare-and-set for default
roles, so a user choice made in the final interval between that read and the
policy write can still race. Disablement prevents future attempts; Windows does
not provide cancellation for an in-flight `DiInstallDriverW` call, so that call
completes and the worker finishes bounded endpoint polling and conditional
route cleanup before it exits. Installer and route restoration failures are
best-effort status, not audio-service startup errors. Driver-property identity
and route behavior still need qualification on a Windows host.

Only `amaterasu.nvhttp` may configure the policy through the internal
`configure_steam_driver_install` action. The global settings editor reads the
structured state through the management `media/audio_driver_status` route,
which requires `media.read`. This is trusted native-module coordination, not a
native-code sandbox.

Enumeration returns `{ok:true,enabled,endpoints:[{id,name,description,adapter_name,default_multimedia}]}`
for active render endpoints. Explicit endpoint IDs come from that list. With no
virtual sink selected, an empty or `default` ordinary endpoint selects the
current default render sink. Regular empty/default opens follow later default
changes; capture-only opens pin the initially selected endpoint. Windows uses
the Console role; Linux uses the Pulse server's default sink.
On Linux, capture reads the selected render sink's monitor source through the
user's existing PulseAudio-compatible server. It never autospawns that server.
The host must run with the interactive user's libpulse server selection and
session environment. The module does not discover or switch to another desktop
session's Pulse server; a host started with a service or stale environment may
connect to a different server or fail to connect.
The default capture uses 48 kHz float PCM and keeps the monitor's actual channel
layout. The optional `capture_format` requests 48 kHz float PCM with 2, 6, or 8
channels; the server performs any channel conversion. Unsupported monitor layouts
fail rather than being reported as a different channel count. Linux enumeration
and capture link against libpulse.

On Linux, `host_audio_enabled:false` creates a uniquely named managed null sink
for capture, then releases it when the last stream closes. An explicit
`virtual_sink` selects an existing sink by exact ID, name, description, or
adapter name. Routed explicit and virtual opens use a shared route lease and
restore the previous default only while it still points at the selected sink;
default and capture-only opens do not change the default. Failed restoration
remains visible in `audio_route_restore_pending` and is retried in the
background. Linux does not install or require a Steam audio driver.
Transient Pulse connection, query, and monitor stream setup failures receive at
most one startup retry only for ordinary default or capture-only opens. Those
attempts do not assign routes or create virtual sinks. Routed explicit and
virtual opens keep their existing single-attempt behavior; permanent endpoint,
format, or access errors remain visible immediately. Each Pulse operation wait
is capped by the bounded startup attempt, each open attempt at 1.8 seconds, and
the total startup retry window at four seconds; the retry delay is 200 ms. This
keeps both attempts and the delay below the stream-session resource-open RPC's
five-second deadline.
Enumeration is limited to 64 endpoints and a 12,000-byte metadata reply. On
Windows, opening an explicit endpoint with `capture_sink_without_routing` omitted
or false temporarily assigns that endpoint as the Console, Multimedia, and
Communications render default, unless it is already the Console default.
Concurrent captures of the same endpoint share the assignment; a conflicting
endpoint open fails. When the final capture closes, the provider restores only
roles it assigned that still point to the selected endpoint, preserving role
changes made elsewhere. If assignment fails, open fails and attempts to restore
roles changed earlier in the same open. A failed restore remains pending and
blocks later routed opens, which retry cleanup first. A Windows background
worker also retries pending restoration once per second. It stops after all
owned roles are restored or after they move away from the selected endpoint;
each attempt checks role ownership again before writing, preserving newer user
defaults. Shared leases still restore only after the final capture closes. The
worker is stopped, unparked, and joined during module shutdown before its
library can unload; a blocked Windows policy COM call can delay unload. Opening
`default` does not change default roles. The provider does not change volume or
mute state.

While an assigned virtual sink is capturing, the worker checks the render
defaults every 100 ms and reapplies that sink if another application changes a
role or Windows leaves a previously selected role unset. This also covers a
virtual sink that was already the Console default at open. The final lease
still restores the defaults captured before the virtual sink was assigned. A
transient policy call failure is retried at the next check.
The pinned Vibeshine path skips its initial role assignment when the selected
sink string equals the host sink string; its encoded virtual format selector
usually differs even when both resolve to the same endpoint ID. This provider
compares resolved endpoint IDs for the already-default case, so that selector
distinction still needs Windows device qualification.

An open with `virtual_sink` selects a managed virtual render endpoint before the
regular `endpoint`. `virtual_sink:null` or omission allows the audio policy to
choose automatically. `host_audio_enabled` defaults to false; on Windows that
policy selects the active `Steam Streaming Speakers` endpoint when host audio is
disabled, or when the ordinary endpoint is empty or `default` has no active
multimedia endpoint. It uses stereo for this automatic virtual selection. If
Steam speakers are unavailable, capture falls back to the requested ordinary
endpoint and reports its actual ID; if that endpoint is unavailable, open fails.
When host audio is enabled and an ordinary endpoint is available, regular
selection is preserved. With no virtual sink selected, an empty or `default`
ordinary endpoint resolves the current Windows Console render default. Unless
`capture_sink_without_routing` is true, the worker checks the Console endpoint
identity every 100 ms and reopens capture after it changes, without changing any
render default role. Capture-only default opens keep the initially selected
Console endpoint across default changes and device-loss retries. Explicit
regular endpoint IDs remain pinned. An explicit `virtual_sink` always takes
precedence and remains strict: it does not fall back when missing. An empty
explicit selector
matches the active endpoint whose adapter name is exactly
`Steam Streaming Speakers`; nonempty text matches an active endpoint's
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
capture closes. Concurrent captures of one virtual endpoint share its selected
engine format. A request for a different channel layout fails with
`virtual_sink_format_conflict` until the final capture releases that endpoint;
recovery of a shared capture keeps the existing format. If the device mix
temporarily differs from the selected layout during recovery, capture waits
and retries whether it owns the format alone or shares it. Shared capture
does not rewrite a peer's format; close cancels the wait. Once the final owner
releases the endpoint, recovery can select its format again. Capture-only
routing is invalid for virtual selection; virtual opens use the normal shared
default-role lease and cleanup.

Set `capture_sink_without_routing:true` on an open to capture the selected
endpoint without changing any render default role. On Windows, when the regular
selector is empty or `default` and no virtual sink is selected, the provider
resolves the current Console default once and pins that endpoint across later
default changes and device-loss retries. The product resolver should set the
flag only for Vibeshine's capture-only case: capture-only enabled, a configured
sink present, no configured virtual sink, and the selected sink equal to that
configured sink. The audio module receives this per-open policy snapshot; it does
not derive eligibility from product settings.

Open/lookup return `{ok:true,stream,endpoint,requested_endpoint,virtual_sink,channels,host_audio_enabled,capture_sink_without_routing,continuous_audio,format}`.
`endpoint` is the selected endpoint ID, `requested_endpoint` is the caller's
regular selector, `virtual_sink` is the requested virtual selector, `channels`
is the resolved requested virtual layout, and `host_audio_enabled` echoes the
selection policy. The actual selected endpoint ID shows whether automatic
fallback was available. An omitted policy field defaults to false for existing
callers. Format contains
`sample_rate`, `channels`, `channel_mask`, and `format:"f32"|"i16"`. Status returns
`{ok:true,enabled,audio_route_restore_pending,virtual_sink_cleanup_pending,streams:[...]}` with only the
caller's streams, optionally filtered by ID. The route flag reports pending
cleanup while Windows retries restoration or Linux retries its default sink;
it clears after restoration or when the target is no longer selected. The
virtual-sink flag reports failed Linux module unloads while the owned cleanup
queue retries them; it clears when the module is gone or no longer belongs to
Amaterasu. Each stream
includes its selected/requested endpoint, format,
`state:"capturing"|"recovering"|"failed"`, `failure`, `discontinuities`, and
`dropped_packets`. After WASAPI reports device loss, the worker reports
`recovering` while it retries the selected endpoint every five seconds. A
Console-default change starts an immediate reopen only for a stream that follows
that default; capture-only default opens stay pinned. Failed reopen attempts
retry every five seconds. Retry delays are cancelled by close or module
shutdown. A reopened capture must keep the published PCM format; an incompatible
format fails the stream with `audio_format_changed`. A temporarily unsupported
mix format during recovery is retried until it becomes compatible or the stream
closes. The first packet after recovery is marked discontinuous, and
sequence/timestamp values continue across
the new capture's timeline. In status, `endpoint` reports the active capture
endpoint and updates after a followed Console-default change successfully
reopens capture;
while recovery is in progress, it remains the last successfully opened capture
endpoint. `requested_endpoint` remains the initial selector. Open/lookup keep
`endpoint` as the initial selected endpoint snapshot, so the stream-session
shared lease keeps its existing identity after a followed Console-default change.
Close returns `{ok:true}`. Missing/foreign lookup or close returns
`{ok:false,code:"stream_not_found"}`; absent status returns an empty list.
Driver error strings and PCM samples are never placed in control replies.

The optional operation ID is 1–128 ASCII graphic bytes. Same-caller retry with
that ID, requested endpoint, virtual selector, layout, and routing policy reuses
the original stream; changing any of those values conflicts. The optional
`continuous_audio` object accepts `packet_ms` values of 5 or 10. Missing or null
keeps legacy capture packet sizes and returns `MEDIA_PENDING` when capture is
idle. An enabled stream combines short capture packets and splits long ones into
frames without changing captured PCM. Frame count uses integer
`sample_rate * packet_ms / 1000` arithmetic and truncates fractional frames, so
44.1 kHz at 5 ms produces 220 frames. At the idle deadline, it zero-pads a partial
frame or produces a full zero frame. It sets `AUDIO_SILENT` only when every
sample in the complete output frame is zero. Full synthetic idle frames use
elapsed monotonic timestamps and set `AUDIO_TIMESTAMP_ESTIMATED`; partial frames
retain captured timestamp flags and QPC timing. Output sequence and timestamps
advance per frame, and capture discontinuity flags are retained.
When a recovered capture marks a discontinuity, a partial frame from before the
loss is padded and emitted first; the recovered samples start a new frame.
Terminal capture failure likewise emits any partial PCM once before reporting
`MEDIA_BACKEND_ERROR`, including after a sink rejects and retries that packet.
The packet duration participates in operation retry identity. After close, retry returns
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
`MEDIA_CLOSED`; terminal device/worker failure returns `MEDIA_BACKEND_ERROR`. A
device-loss recovery remains pending until another packet arrives. A provide/call
grant for `amaterasu.audio/1` is required at the
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

Windows supports little-endian 32-bit IEEE floating point or 16-bit signed PCM,
8–192 kHz and 1–8 channels. It performs no format conversion or resampling.
Linux publishes 48 kHz float PCM, retaining the endpoint's 1–8 channel monitor
layout by default, or using the requested 2/6/8 channel capture format above.
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
counter and does not automatically terminate capture. Legacy opens do not
synthesize packets when a silent endpoint produces no WASAPI packets; enabled
continuous opens synthesize the configured frame cadence. Normal timestamps are QPC
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

Linux route and managed-sink ownership is recorded in the host state's durable
cleanup journal. Startup reconciles pending operations from a previous process,
and background cleanup retries failed route restoration and owned sink unloads.
Shutdown makes one final bounded pass; failed or indeterminate operations remain
journaled for the next startup. Cleanup validates the sink's ownership token before
unloading its module so an unrelated sink reusing a name or module index is retained.

The Windows audio policy/capture behavior and audio capture contract in pinned
Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d` were used as
behavioral references without copying or translating source. The selected-endpoint
capture-only case is represented by the per-open policy above. Continuous audio
is opt-in per open; product setting persistence, management UI, and hardware
qualification remain unimplemented. Opus encoding, configured speaker remapping,
resampling, process-specific loopback, microphone capture, the macOS backend,
and full streaming audio parity remain unimplemented.

Host tests use fake endpoint/capture backends and real native callback bindings
for selection, ownership, idempotency, cancellation rollback, silent packets,
discontinuities, timestamps, queue overflow, sink retry, and close/drop. Worker
coverage checks Console-default reinitialization on a changed endpoint, packet
clock continuity, and strict endpoint pinning when following is disabled. Pure
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

That test has not been run on development hardware. Linux tests exercise Pulse
ABI layout, endpoint selection, channel negotiation, route restoration policy,
and silence packet sizing without opening a server; the ignored Linux monitor
test requires the user's PulseAudio-compatible server. Real Windows qualification
of routed opens must exercise successful switching/restoration, each partial
switch failure and rollback, role changes made by another application, device
removal, default-device changes while capturing, and recovery after restoration
failure. Waveform/channel correctness,
silence, sustained backpressure, discontinuities, and cancellation while drivers
initialize also need hardware coverage. Steam driver setup and conditional role
restoration have cross-platform fake-backend tests; native `DiInstallDriverW`,
administrator access, active endpoint discovery, and Windows route behavior
have not been qualified on a Windows host. An ignored Linux virtual-monitor capture test creates a private null
sink and reads its silence; it requires an accessible per-user Pulse-compatible
server and does not prove physical-device audio. This test has not been run where
the user sound server is inaccessible. Linux physical input/output, route
switching, default changes, device-loss recovery, and PipeWire-Pulse
interoperability still need live host qualification.

Primary API references:

- [Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)
- [Capturing a stream](https://learn.microsoft.com/en-us/windows/win32/coreaudio/capturing-a-stream)
- [GetMixFormat](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat)
- [IAudioCaptureClient GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
- [PulseAudio asynchronous API](https://freedesktop.org/software/pulseaudio/doxygen/mainloop_8h.html)
- [PulseAudio recording and stream APIs](https://freedesktop.org/software/pulseaudio/doxygen/stream_8h.html)
- [PipeWire-Pulse null sink module](https://pipewire.pages.freedesktop.org/pipewire/page_pulse_module_null_sink.html)
