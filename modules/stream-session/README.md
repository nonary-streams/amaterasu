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
   session. When trusted input policy is enabled, it also acquires the session's
   exclusive input lease before reporting success.
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
five mouse buttons, and horizontal or vertical wheel detents. Absolute input is
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

Text, controllers, haptics, ambiguous key encodings, and unmapped keys are
reported honestly without effects. The pending queue holds at most 128 translated
events and sends at most 64 per input request. Overflow, lease expiry, provider
rejection, or any ambiguous batch result fails the session and releases held
input; a batch is never replayed under another operation ID.

Audio endpoints are leased under their resolved identity. Video capture leases
identify provider plus output, not a canonical physical display across providers.
Both are shared read leases. Display mutation, topology restoration and
application launch integration remain separate work.

## Current limits

The initial path requires H.264 SDR, 48 kHz stereo capture, and 5 or 10 ms Opus.
It has no resampler, capture pacing policy, audiovisual clock correction, network
RTSP listener or full Moonlight playback qualification. Real Windows capture,
encoding, and OS input injection still require hardware validation; Linux
providers remain incomplete. The signed synthetic integration tests framing and
real Opus/network composition, not hardware capture or H.264 decoding.

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
