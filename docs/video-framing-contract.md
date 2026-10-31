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

`plan` is synchronous and retry-safe; providers must not commit state or retain
borrows. Provider bindings and callbacks remain live until work retires. The
first version intentionally carries no frame-associated metadata and therefore
does not request or permit HDR metadata insertion. The frame metadata and
single codec-carriage owner are being specified separately; this interface must
not silently drop required metadata or insert it twice.

These contracts and the runtime provider tests are a foundation only. Two
runtime-selected synthetic encoder and framer pairs exercise the same internal
media-transport packetizer and loopback sender; the stream-session pump and production transport service
are not wired to these tables. No
extension profile is admitted to a production stream, and there is no claim of
PyroWave or Dolby interoperability, GPU support, or actual HDR decoding. A
production path requires exact negotiated client/profile matching and independent
encoder, framing and transport support, then an unchanged-consumer provider
replacement test and real client qualification.

## Proposed frame-to-access-unit metadata seam

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
