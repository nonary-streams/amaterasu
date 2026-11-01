# Negotiated video unit and framing contracts

The additive `amaterasu.video-unit.native/1`,
`amaterasu.video-unit-transport.native/1`, and
`amaterasu.video-framing.native/2` interfaces use full encoder codec and framing
namespace/value/schema triples. They do not change legacy `EncodedPacket`,
`VideoApi`, or `MediaTransportApi`. A unit carries a selected stream, offer and
generation, source and configuration generations, access-unit/submission IDs,
and an explicit stream-relative clock. Its bytes are borrowed synchronously;
BUSY consumes nothing. `NegotiatedVideoUnit::validate_for` checks exact expected
identities and structural bounds but does not prove a live session, authorize a
principal, or validate codec syntax.

A framer returns bounded framed bytes, record-start marks, a critical prefix
and at most four FEC blocks. The SDK author adapter checks the input shape,
retains the bound principal, forwards sink status, and validates the plan.
The consumer must independently validate it against trusted negotiated limits;
structural validity and a signed capability are not proof of compatibility.
`VideoFramePlan::validate` checks packet/crypto/network overhead, data-shard
coverage, parity math and 255-shard protected blocks, 1023-shard unprotected
blocks, FEC work and configured queue/wire caps. Every protected block must
meet the trusted host's selected minimum FEC percentage and shard count. Zero
parity is rejected unless the host separately authorizes fallback and the
whole frame is too large for protected blocks within the negotiated block limit;
then all blocks must be unprotected. The packetizer and transport independently
match the supplied minimum percentage, minimum shard count and block limit to
the host's configured values. The validator computes a conservative reservation
before the owner mutates frame index, packet sequence or nonce. Framing v2 is
a separate typed interface: its limits v2 tail preserves the v1 prefix offsets,
but a v1 provider cannot be bound as v2 or bypass the policy checks.
The framer owns neither sockets nor encryption; the transport remains responsible
for packetization, security, delivery, fairness, cancellation and retirement.
Production planned delivery uses the separate
`amaterasu.video-plan-transport.native/2` family. Its selection includes the
complete bounded wire profile: selector, codec, framing, payload type and
encoding name. The transport compares all of them with trusted configuration on
both probe and prepare. The older `/1` ABI remains available to existing SDK
users but does not establish the full wire identity and is not selected by the
production extension route. Its probe is read-only; its prepare derives FEC and packet limits from the transport's own frozen settings.
The stream-session thread calls the selected v2.1 framer probe and copies its
plan before invoking the transport. A provider cannot mutate transport state by
calling the plan sink twice or by returning a malformed plan.

`plan` is synchronous and retry-safe; providers must not commit state or retain
borrows. Provider bindings and callbacks remain live until work retires. The
first version intentionally carries no frame-associated metadata and therefore
does not request or permit HDR metadata insertion. The frame metadata and
single codec-carriage owner are being specified separately; this interface must
not silently drop required metadata or insert it twice.

The opt-in production route currently supports one configured source-driven,
8-bit SDR 4:2:0 extension profile. It requires no reference-frame constraint
and default QP and thread controls because the source probe cannot currently
verify those settings. RTSP requests a fixed encoder configuration at prepare
time. Stream-session asks Video to probe the selected source encoder identity.
Video sends the configured encoder control service a read-only `probe_source`
request containing the exact source ID, encoder identity and configuration. A
positive response must echo those fields and set `ok`, `settled` and `usable`
to true. Unsupported, uncorrelated or unsettled responses fail closed; no
fallback to a generic sibling probe occurs. Video then probes the native source
format, and stream-session binds the selected unit/framer/plan interfaces, and probes the framer and actual
owned transport against host limits before returning an offer. RTSP advertises
only that preflighted profile and accepts only an ANNOUNCE with its selector and
the probed geometry, rate, color and transport parameters. Start rechecks the
profile and configuration, selects the startup-authorized source and services,
and checks the source unit identity and transport limits again before pumping.
Unusable or unselected routes leave legacy negotiation unchanged.

Trusted startup composition must explicitly enable RTSP's `extension_enabled`,
select stream-session's `extension_route`, configure Video's `unit_service`
and per-source `extension_profiles`, and configure media transport's
`plan_service` and `extension_profiles`. The selected profile and service names
must agree. Package grants and dependencies must authorize the unit, framer and
plan native interfaces and the source encoder's broker/native services. The
initiator's inherited broker call grants must include the source encoder control
service as well as Video; native grants are trusted-code authority, not a sandbox.
No installed settings are changed by these interfaces.

Tests use two synthetic source encoders and framers through runtime bindings,
the managed Video module, the stream-session pump and the real media transport
over loopback. Separate tests exercise the RTSP and session handoff. This does
not establish PyroWave or Dolby interoperability, real client compatibility,
GPU support, codec syntax correctness, or HDR decoding.

## Frame-to-access-unit metadata seam

The media owner may define a versioned, bounded frame metadata bundle with one
`EncoderFormatId` schema identity per item, a submission ID, source ID and
source generation, timestamp and matching `FrameClockMetadata`. Reuse
`EncoderFrameOptionsV1.side_data_schema/side_data` when one negotiated bundle
schema can encode the items; otherwise introduce a separate versioned successor.
The capture/processor path preserves this association and the encoder must
report a bounded submission-to-output association when it buffers or reorders.
Unknown required items, stale IDs and conflicting retries fail visibly.

For each selected codec and metadata schema, trusted composition chooses one
carriage owner. With encoder-owned carriage, the encoder embeds the metadata
and the framer never inserts it. With framer-owned carriage, a future versioned
encoder output must carry the same bounded bundle and submission association;
the encoder declares it absent from the bitstream and the framer inserts it
once. Unknown ownership, duplicate insertion and lost required items reject the
frame. The current unit and framing APIs do not carry output metadata and cannot
implement framer-owned carriage; they must not be selected for a session that
requires it. Negotiation, codec byte fixtures, peer support and actual HDR
hardware qualification remain separate requirements.
