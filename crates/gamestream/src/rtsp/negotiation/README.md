# Modern GameStream RTSP negotiation

`Negotiation` is a socket-independent state machine for an already authorized
launch epoch, modern application version >=7.1.431, H.264 SDR, and stereo Opus
with 5 or 10 ms packets. The caller supplies `TrustedLaunch` only after preparing
real transport resources. Its audio/video/control ports are actual server ports;
its session token must be unpredictable, unique to the launch, and retained across
that launch's RTSP TCP connections. This module validates bounds, not entropy.

`handle(epoch, &Request)` returns either serialized RTSP bytes or an `Action`
with a completion ticket. The sequence is:

1. OPTIONS and DESCRIBE against the exact authorized launch URL.
2. SETUP `streamid=audio/0/0`, then `streamid=video/0/0`, then
   `streamid=control/13/0`. The audio request has no Session; subsequent requests
   must carry the exact opaque Session token.
3. ANNOUNCE `streamid=control/13/0`, with `application/sdp`.
4. PLAY `/`.

ANNOUNCE produces a typed SDP action after syntax/profile/capability checks. The
backend **must also compare it with the exact prepared packetization, encryption,
audio configuration, and capture/encoder geometry, rate, reference-frame and color
configuration**. The upper capability limits are not proof that resources match.
Call `complete(epoch, ticket, true)` only after backend acceptance; successful
ANNOUNCE permits PLAY, and successful backend PLAY completion permits Playing.
Backend rejection emits 503 and enters Failed. No request can race a pending
action. Stale epoch/ticket completions cannot change state. CSeq must strictly
increase across accepted requests for the launch, including across connections;
wire retries/replay caching are intentionally unsupported.

TEARDOWN `/` is allowed once audio SETUP has succeeded, including from Failed,
and produces a backend cleanup action. Only successful completion enters Closed.
The caller owns deadlines, expiry, disconnect handling, outstanding backend-job
settlement, and cleanup if the machine is dropped. No timer or destructor here
pretends that asynchronous resources have been released.

SETUP accepts the pinned client's bounded
`unicast;X-GS-ClientPort=50000-50001` form (other valid consecutive port pairs are
also parsed). Those ports are compatibility placeholders, **never authorized
media destinations**. Destinations come from the trusted launch/backend. Responses
use real server ports and control connect data. When `TrustedLaunch` supplies UDP
discovery, the audio and video SETUP responses each carry only that lane's
`X-SS-Ping-Payload`; OPTIONS, DESCRIBE, control SETUP, and public state carry
neither payload. Each payload is exactly 16 base64url characters. It is a
ticket-scoped 96-bit bearer token used by the transport to select the authorized
peer's source port, not cryptographic sender authentication. Discovery payloads
require encrypted RTSP and must be distinct. The media transport freezes each lane
after a valid 20-byte `SS_PING`; this negotiation layer does not accept or inspect
UDP packets.

DESCRIBE advertises H.264 and stereo Opus plus the actual supplied encryption
capabilities. It requests modern control V2 and optionally required media
protection. HEVC and AV1 are advertised only when the caller enables them in
`TransportSupport`: HEVC with Moonlight's `sprop-parameter-sets=AAAAAU` marker and
AV1 with an `a=rtpmap:98 AV1/90000` line. Moonlight prefers AV1, then HEVC, then
H.264 among the codecs both sides support, and reports its choice in ANNOUNCE as
`x-nv-vqos[0].bitStreamFormat` 0, 1 or 2. An ANNOUNCE for a codec the ticket did
not enable fails. It advertises no reference invalidation, surround audio, or
input-device capability. The caller must supply the actual encoder color mode;
there is no implicit BT.601/BT.709 or range conversion. Unsupported ANNOUNCE
features fail rather than being silently downgraded.

When `encrypted_rtsp` is true, the exact launch URL must use `rtspenc://`. The
transport must authenticate incoming CR envelopes with the launch key before
calling this machine, retain replay state across connections, and seal every
response with HR. The boolean itself is not authentication. Plain RTSP is allowed
only when the trusted caller authorizes that launch transport and UDP discovery is
disabled; modern authenticated control V2 is still required. Legacy ENet RTSP, old
stream targets, per-stream PLAY, TCP-interleaved media, alternate content
negotiation, and arbitrary Transport parameters are explicitly unsupported.

Primary behavior was inspected in Moonlight `moonlight-common-c`
`874ac9548f1bd6f095ef2b435c42cdde460e7821`, `src/RtspConnection.c`,
`src/SdpGenerator.c`, and `src/Limelight-internal.h`; the behavioral baseline
`aa292c4cb097f22beab19d6ebf0307367f9bf449` of Vibeshine was inspected using
`git show` for its RTSP description/dispatch behavior. No upstream implementation
was copied. Tests use the primary-shaped SDP fixture with negotiated V2 enabled,
raw fragmented RTSP request bytes, exact target/order/session checks, rejection
paths, and delayed backend completion rather than mocked ready responses.
