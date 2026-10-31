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

The authority accepts only one selected direct effect owner. `FeedbackLease`
holds a single aggregate ticket for a source, stream, device generation and
fixed set of locally negotiated feedback capability identities. Messages are
ordered work under that lease; they do not consume one pairing record per block.
`Plane::send_source_feedback` checks the native stream handle, controller slot
and message kind against the immutable lease scope before calling the transport.
These local schema identifiers are not client-wire negotiation tokens. A
protocol adapter must verify the negotiated intersection and install the scope.

The local lease accepts a bounded exact retry record for one busy message. A
later message cannot overtake it, and a retry with altered bytes, capability or
identity is rejected. Known-busy work can be explicitly cancelled; a terminal
or uncertain send consumes its sequence and the lease continues to cover any
queued work. The owner must bound all upstream queues and keep stop controls
until accepted or explicitly cancelled during retirement. The owner must bind
exactly one lease per authorized source/stream, deduplicate operation identity,
and fence locally when status requests drain or an authority/owner epoch changes.
Polling can delay observation: the trust mutation remains uncommitted during
that interval. On observing the fence the owner stops new messages, joins active
callbacks, resolves pending busy work, retires the whole stream, reconciles the
aggregate ticket and acknowledges the matching pairing drain. Already handed-off
packets cannot be recalled.

The real-ledger/native-callback tests drive 300 feedback enqueues through one
pairing record, busy stop retry and cancellation, mismatched capability/slot,
replay, a stale status, a lost completion response and a blocked native call
during revocation. Those tests use a synthetic retirement receipt. A separate
control-provider test combines the real ledger, lease and actual provider
retirement receipt while staged datagrams and reliable ENet work are discarded.
The other control-provider tests cover an in-flight local network handoff. These
tests do not establish the full broker/lifecycle/transport integration.

The current pairing `Admit` does not record or return the capability and stream
scope. The exact additive request and response needed from the pairing owner are
in [the authority request](input-feedback-lease-authority-request.md). Until it
is implemented and the SDK checks its response, the local lease scope is a
trusted test assertion and source binding stays closed. Production also needs
the authenticated lifecycle join, the selected owner's revoke/worker-retirement
handshake, restart recovery and an independent review before activation.

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
