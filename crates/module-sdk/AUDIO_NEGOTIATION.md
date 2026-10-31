# Negotiated encoded audio

The legacy `amaterasu.audio-encoder.native/1` table continues to carry Opus.
The separate `amaterasu.audio-encoder-negotiated.native/1` family supplies
bounded exact-format offers and a packet contract for a consumer that opts in.
The SDK `audio::negotiate` helper matches a caller-ordered allowlist against
provider offers, selects an exact generation and offer, and reports stale or
unsupported selection without guessing a fallback. Each packet is checked
against its stream and selected format before its borrowed bytes are copied.
Keep the provider binding alive until the stream and all work are retired.

Standard identifiers describe Opus packets and AC-3/E-AC-3 syncframes; a
nonzero namespace can identify an independently agreed schema. Descriptor
validity does not prove codec syntax, encoder capability, protocol negotiation,
packetization, client support, or hardware availability. A consumer must
explicitly understand the exact format and channel-mask schema. Codec-specific
config bytes are bounded and copied during offer enumeration. Packets are
bounded, borrowed only in the callback, and remain queued on sink rejection.
These contracts do not change the existing Opus-only stream-session or wire
transport; using another codec requires a compatible protocol/transport pair.
