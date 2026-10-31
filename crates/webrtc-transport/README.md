# Browser WebRTC transport foundation

This independently built library wraps the pinned `str0m` WebRTC
engine and its AWS-LC cryptographic provider. It accepts a browser SDP offer,
produces an answer with an explicit host UDP candidate, drives ICE/DTLS/SRTP
and data-channel events, and sends complete H.264 Annex B access units using
packetization mode 1 and Opus
packets. Users of the low-level `BrowserTransport` must bind and own the socket, authenticate signaling, route
received datagrams, obey deadlines and the drain-before-mutation contract, and
supply already encoded media. The caller must check the selected H.264 payload
format against the encoder before sending. Timestamps are stream-relative
microseconds and must advance after conversion to the codec clock.

`BrowserSessionSet` adds an optional owner-scoped, nonblocking UDP session
driver. It binds a concrete address and ephemeral port per session, defaults to
loopback-only policy unless remote use is explicitly enabled, and requires an
identity supplied by a verified authentication context. Exact operation retries
return the same answer; closed operations remain tombstones. The set limits
sessions, operations, work per tick and queued input events; it expires idle or
incomplete peers. The embedding authority must tick sessions, consume events,
close them on revocation and roll back newly created sessions if its enclosing
request fails. This identity is not itself authentication. No management route
or media submission is connected to this driver. Input message size is checked
after the WebRTC engine assembles the message, so it is not yet a complete
memory bound for untrusted remote peers.
The operation limit is lifetime-wide for a set: after 128 distinct operations,
new operations are rejected until the set is replaced. A durable bounded retry
policy is still needed before exposing it as a long-running service.

This is a transport building block, not a browser-streaming service. It does not
authorize a user, select a capture source, own a launch or
input lease, enforce a data-channel input schema, or implement reconnect and
cleanup across host components. It exposes engine events to the embedding owner;
that owner must impose bounded input, output, pacing and resource policies before
accepting remote clients. HEVC, AV1, HDR, TURN and multi-track negotiation are
not implemented here. The only enabled video/audio formats are H.264 and Opus.
Native modules remain trusted code.

The `browser-loopback` and `session-loopback` examples are test fixtures, not a
service: they bind only an ephemeral IPv4 loopback socket and read one offer
from stdin. The media fixture sends a generated 128×128 H.264 test frame and
Opus silence. The fixture frame exceeds a single
RTP packet and was generated locally
with FFmpeg's libx264 baseline encoder. To exercise actual Chromium decode and a
data-channel message in a temporary browser profile on Linux:

```sh
TMPDIR=/var/tmp python3 scripts/test_webrtc_browser.py
```

The second fixture checks that the managed session receives a browser data-channel
message and closes. That test requires Chromium, Node, `unshare`, `ip`, and permission to create a
user and network namespace. It confines the browser and peer to a namespace with
only loopback enabled. The test proves loopback protocol progress and
browser decode only. It does not exercise signed host integration, real capture,
remote networks, game input, display ownership or other operating systems.

References: [str0m](https://github.com/algesten/str0m), including its documented
single-mutation drain invariant, and [the WebRTC API](https://www.w3.org/TR/webrtc/).
