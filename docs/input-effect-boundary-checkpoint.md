# Input and feedback effect-boundary checkpoint

Source-scoped route installation and effects remain disabled. The pairing-owned
admission and drain contract has been imported, but its authenticated lifecycle
join and effect-owner integration are not complete. A source label is not authority.

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

## Effect-owner state and remaining tests

`effect_authority::consumer::EffectTicket` constructs the pairing authority's
requests, rejects mismatched replies, preserves uncertain admissions for lookup,
and holds queued or uncertain feedback until an exact whole-stream retirement
receipt. It does not authenticate the broker reply, schedule requests, or install
a route; trusted composition must select the authority and transport providers.
Sequences are unique and monotonically increasing across the entire authority
epoch, not merely a device. Completion is valid only after the provider's
asynchronous work has retired as well as its synchronous call.

The input and native feedback boundary tests compile the pairing provider's
actual ledger and hold its ticket across the controller fake-backend injection
and the native transport callback. The feedback test uses a synthetic transport
receipt: the separate control-transport test validates actual ENet queue
retirement, including a blocked local send. These are not a combined production
pairing/broker/lifecycle/transport test and do not prove an authenticated source
join. `stop_discards_busy_feedback_before_transport_retry` covers a separate
plane queue. Lost admission, cancellation and mismatched receipts are covered by
SDK state-machine tests.

The authority accepts only one selected direct effect owner. That owner must
retain a ticket while an input request runs through the broker and while any
feedback remains in the selected transport. Holding each feedback ticket until
whole-stream retirement is safe but exhausts the authority's 256-record limit
in normal operation. Production activation therefore requires an independently
verified bounded completion or cancellation mechanism (or another coherent
batching design), and a real broker integration proving the complete fence,
worker and transport drain barrier. Keep the route disabled until then. Tests
must use loopback or injected socket fakes and cannot claim remote delivery.

`amaterasu_module_sdk::input::extensions::source_label::observe` parses the
negotiated lifecycle source label against an expected stream target only. It is
not connected to `session_binding::AcquireJob` or `RoutePolicy::install`; no
production source effect is enabled. The route still needs distinct stream,
source and device incarnation fields, and the app worker's final admission
operation must supply the required authority and drain semantics before wiring.

## Prepared whole-stream retirement seam

The control transport now offers an optional owner-scoped `retire` and
`retirement_status` broker operation with a provider epoch and a replayable
retirement operation ID. Exact JSON, ownership and reply semantics are in
[the control transport contract](../modules/control-transport/README.md#optional-whole-stream-retirement-receipt),
and `amaterasu_module_sdk::transport::control_retirement` supplies the request
and receipt types. Its scope is `whole_control_stream`: it destroys the ENet
peer and its queued datagrams and retransmissions, not one source. The provider
serializes the local send callback, socket handoff and stream removal with the
same controller mutex. The receipt says this stream will issue no future Network
SDK sends; it does not recall earlier socket handoffs or establish delivery or
independent Network-provider worker retirement. Provider restart changes the
epoch and makes prior receipts unresolvable, so recovery must remain fenced.

The protocol authority owner must first fence new admissions and then have the
actual stream-session owner invoke this operation on the selected control
provider using the epoch saved from that stream's open response. After any lost
response, query `retirement_status` with the same retirement operation ID and
provider epoch and verify the exact stream, operation, epoch, state and scope.
Only combine that receipt with settled input/backend work and the app worker's
revocation barrier. Do not grant a protocol caller ownership of another module's
stream, treat a label as authority, or fall back to a bare close/status when a
receipt is unavailable. This checkpoint does not connect any production route
or acknowledge source revocation. The authenticated protocol binding and combined barrier remain required.

The new control-provider tests use only loopback and cover unsent datagram
retirement, rejection of late native sends, owner and epoch mismatch, status
after a lost broker reply, and a local send held in flight while retirement waits
for the controller lock. Their one observed datagram was handed off before the
retirement receipt; the test makes no claim that it can be recalled. A selected
replacement transport must independently implement the same contract and pass
an unchanged consumer test before it is relied upon.
