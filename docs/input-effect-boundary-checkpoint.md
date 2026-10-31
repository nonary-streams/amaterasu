# Input and feedback effect-boundary checkpoint

This checkpoint prepares tests only. Source-scoped route installation and effects
remain disabled until the app/lifecycle owner delivers the protocol-owned effect
admission and revoke/drain API. A source label is not authority. The test locks
below are local test doubles, not a production gate or a second authority.

## Exact boundaries

- `modules/input/src/controller.rs::deliver_events` invokes its existing
  `FnMut() -> InputResult<()>` guard and then calls `inject_batch_event` in a
  separate operation. A successful check does not itself retain a ticket across
  the backend effect. The real admitted permit must remain owned through each
  injection, including any asynchronous native work required by the provider's
  retirement contract. The fast non-keyboard path and shortcut path both need
  this boundary; queued work, repeat, contact release and cleanup must not bypass
  it. Scope the guarantee to source-owned work without changing unrelated legacy
  input semantics. `modules/input/src/lib.rs::advance_revoke` currently checks
  `controller.cleanup_required()`; it does not know about future protocol-owned
  tickets or callbacks and cannot acknowledge them as drained yet.
- `modules/stream-session/src/plane.rs::pump_feedback` pulls queued feedback and
  calls `Backend::send_feedback`; `Plane::send_rumble` is a separate route.
  Source-scoped feedback needs authenticated route/generation metadata retained
  with the queued item until the exact final send. Pause/revoke must reject new
  source items and remove or fence pending ones before acknowledging drain.
  Ordinary session HDR metadata also uses `send_feedback` from `native.rs` and
  must not be accidentally gated as controller feedback. No video or media
  behavior is changed by this checkpoint.
- `NativeBackend::send_feedback` calls the selected `ControlTransportApi::send`
  synchronously, but the API contract says success is local admission only.
  `modules/control-transport/src/stream.rs::send` seals and enqueues into ENet;
  `stream.rs::tick` later calls `stream/socket.rs::flush_socket`, which performs
  `UdpSocket::send_to`. ENet can also retransmit reliable messages. Therefore a
  ticket released when `send_feedback` returns does **not** fence the actual
  socket send or later retransmissions. The selected transport needs an
  explicitly versioned, testable send/retirement contract that tracks or fences
  the source's queued datagrams and retransmissions until sent or cancelled, or
  must close and retire the whole affected stream before acknowledging revoke.
  The current ABI gives no per-message send completion/cancellation proof. An
  already handed-off kernel datagram cannot be recalled; the guarantee is no
  new local socket handoff after a completed barrier, not no remote arrival.

## Prepared tests and remaining tests

`controller::tests::test_ticket_harness_holds_drain_until_actual_input_backend_returns`
blocks the actual controller's fake backend `inject` call, and
`plane::native::tests::test_ticket_harness_holds_drain_until_native_feedback_send_returns`
blocks the actual native transport callback. Their `RwLock` is a deterministic
test-only stand-in, and their assertions prove only that the test harness holds
its simulated ticket across the synchronous boundary. They do not test the
upcoming app authority or socket flush. `stop_discards_busy_feedback_before_transport_retry`
checks the existing plane queue is discarded on stop after a busy response; it
does not retire a datagram already accepted by the transport.

After the authority API lands, replace the stand-in with the real ticket and
exercise actual lifecycle/protocol/input/feedback integration over the broker.
Required races: revoke before ticket acquisition, while backend injection is
blocked, while native send is blocked, after ENet admission but before socket
handoff, while a socket send is blocked, during retransmission, and on lost
admission or release replies. Assert fence immediately, no new effects, drain
ack only after all admitted effects and callbacks retire, and no stale source,
stream, device or boot generation is accepted. Cover busy, cancellation,
expiry, failed cleanup, worker restart, independent sources and all queue limits.
Tests must use loopback or injected socket fakes and must not claim remote delivery.

`amaterasu_module_sdk::input::extensions::source_label::observe` parses the
negotiated lifecycle source label against an expected stream target only. It is
not connected to `session_binding::AcquireJob` or `RoutePolicy::install`; no
production source effect is enabled. The route still needs distinct stream,
source and device incarnation fields, and the app worker's final admission
operation must supply the required authority and drain semantics before wiring.
