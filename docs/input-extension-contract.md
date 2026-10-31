# Optional input extension boundary

`amaterasu.input-extension/1` is an optional, separately composed broker service.
The Rust SDK in `amaterasu_module_sdk::input::extensions` supplies a bounded
message type, a consumer submit helper and a provider adapter. It does not
change the v1 input lease, GameStream wire format, or native ABI. Nothing in
this contract enables an OS device, a network protocol or a client feature.

The trusted lifecycle/composition owner installs a `RoutePolicy` for each
host epoch, session, authenticated source, logical device and generation. It
supplies separately authorized immediate caller and initiator pairs for traffic
to the device and to the client. Neither route values in a request nor a
capability name grant access. This adapter checks both broker-authenticated
identities and an exact live route, and it rejects expired requests. The caller
must use the broker's cancellation check before dispatch. Composition is
responsible for authenticating the lifecycle evidence, binding it to the input
lease and client, and retiring a route before those rights end. There is no
production lifecycle integration in this increment.

Each capability is an exact namespaced schema, version and direction. Installation
intersects provider and protocol-adapter offers, verifies required capabilities
are in the intersection, and asks the endpoint whether it implements each of
its advertised capabilities. This is a provider assertion, not independent
proof of behavior. Unknown required capabilities must be rejected by either
actual implementation before they are offered. Protocol negotiation with a real
client and per-device availability checks remain the respective adapters' jobs.

A message carries a monotonically increasing sequence per route and direction
and at most 2,048 bytes. The maximum serialized request is 16 KiB; route,
capability and authorization lists are bounded. The provider validates its
schema-specific payload, ordering and lifetime. For example, an independently
versioned schema could carry validated Steam controller attributes or a bounded
speaker PCM block with its format, timing, stream start/end and ownership
metadata; these example schemas and their wire encodings are not defined here.
No universal opaque payload interpretation is implied. The sender and receiver
must agree on the exact schema and its edge cases; they must not reinterpret
unknown payloads as another schema.

`Endpoint::submit` must return busy without consuming the message. Successful
submission means only local admission, not OS injection, remote delivery or
playback. Endpoint-owned queues must be bounded and must independently preserve
required stop/neutralization, expiry and worker-retirement semantics. The
adapter advances sequence only on successful local admission. Broker retry
records and operation IDs must be reconciled for ambiguous replies; retrying a
potentially accepted message under a new operation ID is not idempotent. The
adapter itself does not persist admitted messages or cancel already-admitted
work. Native providers remain trusted code.

The owner serializes dispatch, installation and retirement. Retirement fences
new admission and does not cancel already admitted work. A replacement for the
same logical identity must use a higher generation; tombstones are retained for
the adapter lifetime and bounded to 32 identities. Restart recovery and
host-epoch generation require trusted lifecycle coordination and are not
implemented by this adapter. When its bound is exhausted, the adapter rejects
new identities rather than evicting tombstones and risking stale reuse.

`crates/runtime/src/input_extension_tests.rs` exercises two synthetic providers
through the real selected broker, including both directions and unauthorized
callers, unknown capabilities, stale routes, oversized payloads, sequence and
retirement. It neither loads a production provider nor proves the actual
Steam/speaker wire protocols, driver access, timing or client interoperability.
The existing `authenticated_controller_extension_packets_reach_the_native_input_contract`
stream-session test covers arrival, touch, motion and battery through the
production pipeline's explicit pre-translator path; the standalone translator's
rejection of those events is not a production-path rejection.
