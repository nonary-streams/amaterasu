# Next slice: negotiated access units and packet plans

This is a proposed follow-up, not implemented by the profile-negotiation slice.
Coordinate its shared files with the active media worker before assignment.

1. Add a versioned encoded-unit ABI alongside `crates/media/src/lib.rs`'s legacy
   `EncodedPacket`. Carry the same `namespace_id/value/schema` fields as both
   `media_profile::FormatId` and `encoder_provider::EncoderFormatId`, plus
   session and configuration generation, complete-access-unit boundaries,
   timestamp, flags and bounded optional typed side data. Add checked lossless
   conversions; do not encode extension identities in the legacy `codec: u32`.
   Provide SDK author/consumer adapters and retain the binding until the last
   callback/queued unit is retired. Existing `VideoApi` and `MediaTransportApi`
   layouts stay intact.
2. Replace the closed-only bridge in `modules/video/src/plane/provider_output.rs`
   and `modules/video/src/queue.rs` with an additive V2 route that preserves the
   selected codec and framing IDs, ownership, timestamps, bounded queue and
   synchronous retry semantics. Keep the existing V1 route for standard clients.
3. Define a selected, versioned framing service that receives a bounded complete
   unit, negotiated profile and packet budget. It returns validated ranges,
   record-start flags, critical coverage and FEC block parameters; it does not own
   sockets, RTP sequence numbers, keys or nonces. Wire this into an additive
   route in `modules/media-transport/src/{model.rs,stream.rs,controller.rs,native.rs}`
   and `crates/gamestream/src/video.rs`. Validate the entire plan, total work,
   encryption overhead, range overlap, FEC shard/block limits and queue capacity
   before consuming sequence numbers or nonces. Keep FEC execution and network
   sending with the host's selected Network SDK provider. A busy result consumes
   nothing and is not remote delivery.
4. `modules/rtsp/src/controller/{record.rs,network.rs,workflow.rs}` and
   `modules/rtsp/src/model.rs` currently carry only `VideoStreamFormat`. Integrate
   `Action::ExtensionAnnounce` only after encoder, framer and transport support
   have been selected and independently checked for that exact descriptor;
   propagate immutable session/epoch and generation through lifecycle. Preserve
   fail-closed behavior if any component is missing, changes or cannot start.
   NVHTTP advertisement and capability discovery require separate coordination
   with its current owner. Built-in standard profiles must then use the same
   provider/SDK contract, not a hidden default implementation.
5. Tests must run unchanged consumers with two independently authored providers
   selected through runtime grants: simulate actual client DESCRIBE/ANNOUNCE,
   submit a synthetic nonstandard access unit through V2 and check loopback UDP
   packet bytes. Cover unoffered and mismatched IDs, malformed/excessive plans,
   queue backpressure, sequence/nonce atomicity, cancellation, provider teardown,
   grant denial and legacy behavior. Actual PyroWave framing/version, peer decode,
   GPU support and Dolby transport are separate tests and implementations.
