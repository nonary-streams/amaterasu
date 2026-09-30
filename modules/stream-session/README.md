# Managed stream sessions

This library coordinates caller-owned media sessions. The host remains a generic
module shell; session policy, provider selection, native media forwarding and
cleanup live here. The service is `amaterasu.stream-session/1`.

It is disabled by default and requires an explicit caller allowlist. Native
libraries are trusted code; broker grants do not sandbox their machine code.
The caller must already have authorized the peer and supply fresh session keys.
This library does not implement pairing, NVHTTP launch or a network RTSP server.

## Lifecycle

1. `prepare` allocates a session record, a stereo Opus encoder, media UDP sockets
   and an authenticated control endpoint. It returns the bound ports and actual
   encoded audio packet size before opening capture hardware.
2. The client completes the ENet handshake and proves possession of the control
   key. `start` checks authenticated control before acquiring capture resources.
   A premature start leaves the prepared allocation available for a retry.
3. `start` opens video with explicit access-unit boundaries, opens audio, validates
   48 kHz stereo PCM, acquires shared capture leases and activates the coordination
   session. `host_audio_enabled` is snapshotted with the start configuration and
   passed to AudioCapture for sink policy. When trusted input policy is enabled, it
   also acquires the session's exclusive input lease before reporting success.
4. `stop` releases held input, stops new capture, drains video and Opus, waits for
   the local transport queue to empty, closes providers and releases session
   leases. This confirms local cleanup, not client receipt or playback.

`lookup` reconciles a caller's prepare operation ID. Reusing the same ID with a
changed configuration is rejected. `status` and `close` are owner scoped; status
lists are bounded and report truncation. Two live sessions and 128 lifetime
operation records bound resource use. Closed records preserve retry identity.

Failed or cancelled operations unwind allocated resources. Uncertain broker
requests are settled before operation lookup. A video close response can mean
cleanup has only begun: the coordinator waits for lookup to confirm absence.
Unconfirmed cleanup remains visible as `cleanup_failed` and retains lease
ownership; a later stop can retry it.

When prepare includes a frozen frame-limiter snapshot and authoritative game
executable path, start applies that policy before opening video. A verified
lease stays with the stream session and is restored after media closes, including
failed startup and bounded module shutdown. Provider unavailability does not
prevent streaming; `frame_limiter_applied` is true only for a verified effect.
Playnite launches currently lack an authoritative game executable path, so they
cannot safely apply a per-game limiter profile through this path.

Orderly stop must run before module replacement. Unload releases native bindings
and attempts to settle pending broker requests; it cannot drive asynchronous
provider cleanup after ticking ends. Provider unload/process shutdown remains the
backstop for active resources. This is not a claim of seamless live replacement.

## Media and control

The native plane has separate bounded video, PCM and Opus queues. Backpressure
stops upstream reads. H.264 fragments must share a sequence and timestamp and end
with the opted-in access-unit flag. Assemblies are capped at 8 MiB and 1,024
fragments. Video and audio EOS remain distinct from frame boundaries.

Only authenticated control plaintext reaches the decoder. Modern client IDR and
reference-invalidation requests coalesce into a video keyframe request. Ping and
start messages are recognized. ENet channel and delivery metadata do not confer
authority.

Input injection is a trusted startup policy. `enable_input` defaults to `false`;
when enabled, `input_lease_ttl_ms` must be 1,000 through 30,000 milliseconds. The
coordinator acquires input as its own private operation after control
authentication and session activation, renews the lease conservatively, and
never accepts a caller-supplied lease or operation identity. Input received before
the session becomes active is discarded without changing keyboard/button state.

The active path supports the fixed Moonlight virtual-key mapping, relative mouse,
five mouse buttons, and raw horizontal or vertical wheel units. Absolute input is
enabled only when the video provider returns a valid `desktop_geometry` snapshot
for the captured output and its containing virtual desktop and the acquired input
backend reports absolute-pointer support. The stream dimensions never substitute
for those physical bounds. An absent snapshot or missing backend capability leaves
absolute input unsupported; malformed provided metadata fails startup.

The video provider checks the complete geometry before activation and on each
configured capture cadence. A change or loss closes native video delivery; the
session discards controls collected by that failing pump and releases input and
all owned resources. This bounds detection to the capture cadence and scheduler
latency, but does not make display hotplug and input dispatch atomic. Per-monitor
DPI behavior still requires hardware qualification.

Controller state uses the acquired backend capability. Touch and pen require its
native policy and capability plus validated `touch_port` metadata from video;
without that metadata, packets are counted as unsupported and streaming continues.
Text, haptics, ambiguous key encodings, and unmapped keys are reported honestly
without effects. The pending queue holds at most 128 input
events and sends at most 64 per input request. Overflow, lease expiry, provider
rejection, or any ambiguous batch result fails the session and releases held
input; a batch is never replayed under another operation ID.

Audio endpoints are leased under their resolved identity. Video capture leases
identify provider, constant-rate mode, and output, not a canonical physical display
across providers. Both are shared read leases. Display mutation, topology
restoration and application launch integration remain separate work. The optional
`constant_rate` start field is accepted only with WGC and defaults to false.
The optional `device_name` start field supplies session-history display metadata;
it is independent from the `output` capture source. Older callers that omit it
continue to record the capture output as the device name.

Session history is an optional asynchronous side path. Each session retains at
most one in-flight history request and eight queued updates. Active sampling runs
once per second; confirmed teardown also queues a final cumulative sample when
counters changed since the last scheduled sample, so short streams retain their
media totals. If no monotonic millisecond elapsed, the sample reports finite
zero rates because the schema has no unknown representation; the
`telemetry_limits` event records that rate limitation. An unsent sample is
replaced by the latest cumulative sample if the history service is slow. At the
eight-update bound, a lifecycle event can evict a queued sample; a new update is
dropped only when all slots hold non-coalescible events. Lifecycle events remain
ordered and are sent between session_begin and session_end. A timeout,
unavailable service, result failure, or storage error
does not affect media activation, pumping, or teardown. Stopping settles any
outstanding history request before advancing its queued transition events and end
operation. Shutdown makes a bounded cancellation and result-release attempt; if
teardown was already confirmed, it also makes a bounded best-effort flush of
queued updates in order followed by the stable session_end operation.

Sample frame counts are complete non-EOS video access units accepted by the
media transport's bounded local queue. bytes_sent counts encoded video and
audio payload bytes accepted at that same boundary; it excludes wire framing,
FEC, encryption overhead, and client delivery. dropped_frames counts only
forward sequence gaps observed between accepted video access units, not
end-to-end loss. FPS and bitrate use cumulative accepted counters and
monotonic-time deltas. The video ABI exposes no encode timing, so
encode_latency_ms is the schema's numeric zero; a one-time telemetry_limits
event marks this and end-to-end drops as unavailable and explicitly says local
transport acceptance does not prove client delivery.

## Current limits

The initial path requires H.264 SDR, 48 kHz stereo capture, and 5 or 10 ms Opus.
It has no resampler, audiovisual clock correction, network RTSP listener or full
Moonlight playback qualification. WGC constant-rate delivery is opt-in and repeats
the last owned frame at the stream cadence; it does not provide automatic capture
selection or context-aware frame pacing. Real Windows capture, encoding, and OS
input injection still require hardware validation; Linux providers remain
incomplete. The signed synthetic integration tests framing and real Opus/network
composition, not hardware capture or H.264 decoding.

## Arming after RTSP negotiation

`start_when_authenticated` accepts the same session and configuration as `start`
but returns `armed` before capture opens. The accepted lifecycle then checks
control authentication every 100 ms within the original prepare deadline. Once
proved, it uses the existing bounded startup and cleanup workflow. Identical
retries do not extend that deadline or create another allocation. Ordinary
`start` still rejects an unauthenticated control peer without arming.

This ordering matters because Moonlight completes RTSP PLAY before opening its
control connection. PLAY can acknowledge arming without claiming media is active.
Owner cancellation or lost completion before the first armed response triggers
rollback; cancelling a later retry cannot revoke the already accepted lifecycle.

## UDP peer discovery

Fixed destinations remain supported by supplying both nonzero `video_port` and
`audio_port` values to `prepare`. For negotiated destinations, omit both ports and
supply:

```json
{
  "discovery": {
    "video_ping_payload": "0123456789abcdef",
    "audio_ping_payload": "fedcba9876543210"
  }
}
```

Each payload must be a distinct 16-character unpadded base64url value generated
by the trusted caller. Stream-session forwards this object unchanged to the media
transport but never includes it in status or lookup replies. The trusted peer IP
is still mandatory. A missing port without discovery, a configured port with
discovery, or a zero port is rejected rather than treated as a client-provided
placeholder.

Prepare returns the bound local UDP sockets immediately so RTSP SETUP can publish
them and the client can send discovery probes. Status reports `peer_discovery`,
`media_ready`, and the nullable learned `video_peer_port` and `audio_peer_port`.
Capture remains closed until control authentication and both media lanes are
ready. One discovered lane is insufficient. Background checks retain the original
prepared deadline; retries do not extend it. Fixed-port sessions are ready
immediately and preserve the previous startup behavior.

The nested `start` and `start_when_authenticated` video configuration accepts
`max_reference_frames` values `0` and `1`, defaulting to `0`. Zero preserves the
legacy unconstrained request and is omitted from serialized configuration for
compatibility with older strict video providers. One requires a single H.264 reference frame and is
forwarded unchanged through video to the encoder; invalid values fail start
validation before capture allocation. Video does not activate unless the encoder
confirms the selected constraint. On Windows the encoder uses Microsoft's
[`CODECAPI_AVEncVideoMaxNumRefFrame`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideomaxnumrefframe)
property. Hardware and emitted-bitstream qualification remain outstanding.

The nested video configuration also carries `max_batch_size_kb` (default 64,
range 0..=64) to the media transport. Values 0 and unsupported in-range values
normalize to an effective 64; 16, 32, and 64 select the software per-frame send
allowance. Eligible batches attempt Windows UDP segmentation or Linux UDP GSO;
unsupported sockets fall back to individual datagrams. Live NIC and Linux
qualification remain outstanding.

Encoder selection preserves legacy unsigned startup ordinals and accepts a full
stable encoder identity or the exact `"auto"`/`"software"` backend selectors.
The selection passes unchanged to video, which resolves non-identity selectors
once before capture allocation. Missing software support fails explicitly; fixture
control success does not qualify real capture, encode, or client playback. See
[video selection policy](../video/README.md) for bounds and automatic-selection
limits.

Software tuning remains exact through the launch snapshot: nullable `sw_preset`
and `sw_tune` inherit defaults, while explicit empty strings are retained.
`slices_per_frame` carries Moonlight's positive negotiated H.264 slice count;
`min_threads` carries the reference signed 32-bit lower bound (default 2). The
software x264 backend requests their maximum for x264 thread count, sliced-thread
mode, and slice count; x264 may cap effective values for frame dimensions and
backend limits. QP defaults to 28 but remains metadata because x264 ABR
does not consume it and this build has no VAAPI CQP consumer. The backend policy
supports `auto` and `software`, and real codec availability remains a separate
requirement. [NVHTTP policy](../nvhttp/README.md#software-encoder-policy)
documents persistence, scope precedence and platform qualification limits.
