# Feedback source join: production contract and remaining gates

Production binding remains closed. The optional NVHTTP join in authority commit
`76eae125c05b6de1bbe085bae6918f5768fa3f13` and the stream owner do not yet
form a revocation-safe end-to-end source route. Pairing and NVHTTP both return
`source_attestation_unavailable` outside tests. No installed grants, active
streams, hardware devices, or production gates are changed by this checkpoint.

## Input-provider observation

The existing owner-authorized `feedback_peek` response now adds these fields to
each event:

```json
{"sequence":1,"device_generation":2,"device_capabilities":32768,
 "feedback":{"type":"rumble","controller":3,"low_frequency":0,"high_frequency":0}}
```

The selected input provider allocates `device_generation` per controller slot,
monotonically across input-lease replacement for that provider process. It fences
an old slot's callback producers before Linux detachment on a new arrival or an
inactive controller mask. Already queued feedback remains tagged with its old
generation until acknowledged or the input lease ends, so a reliable stop is
not coalesced into a replacement device's records. Lease termination still
clears the existing queue, and remote stop delivery is not guaranteed. Other
slots remain independent. Generation exhaustion fails closed until provider restart.
The feedback lease, provider boot epoch, lifecycle source and controller slot
must all be retained with this number; the number alone is neither globally
unique nor an authorization. The ordinary peek response does **not** convey an input boot epoch or lifecycle
source label; those require the separate observation below. `device_capabilities` is the
capability value observed at producer creation and can originate in the client's
controller-arrival packet; it is a claim, not proof of device, driver, client
protocol or policy support. The stream consumer now checks tagged feedback for monotonically increasing
per-slot device generations and consistent capabilities before enqueueing it;
legacy providers without the fields remain supported. It fails closed on stale
or conflicting tagged records. On the test-gated scoped route, controller re-arrival, active-mask removal and
haptics disable fence the local source and stop its stream before the event is
forwarded to the input provider. Pairing fencing and whole-stream retirement
then proceed asynchronously; the legacy production route is unchanged.

The selected stream owner can now request a bounded, versioned observation from
the selected input provider:

```json
{"action":"observe_feedback_source","version":1,"lease":"<owned input lease>",
 "input_binding":"<lifecycle binding>","controller_slot":3}
```

Only the direct startup-selected feedback owner caller/initiator is accepted;
its default is `amaterasu.stream-session`. The
provider requires an active owned lease, lifecycle binding, actual selected
backend slot, and exact current device incarnation; it validates the lifecycle
binding again with `source_version:1` and its boot epoch and rechecks the local
state while the request is in flight. A successful selected-provider response
returns `{ok:true,version:1,target,source,input_boot_epoch,device:{slot,
generation,capabilities},provider_offer,immutable_controller_policy}`. The offer
contains only schemas actually produced by the selected live Linux device: DS4
rumble/RGB, DS5 rumble/RGB/adaptive triggers, and PCM only when the DS5 gadget
transport is active. Other providers currently report an empty offer. The policy
flag is true only for an explicitly enabled per-lease controller override; a
lease inheriting mutable global policy is not treated as immutable. The stream
adapter intersects that offer with authenticated RTSP flags, the per-controller
PCM bit, controller support and the authenticated haptics enable command. An
empty intersection grants nothing, and unknown schema offers are rejected by
this built-in adapter. This observation remains point-in-time; it does not fence
policy changes or admit a lease. It contains no certificate. The lifecycle source
may be unavailable until activation; absent, mismatched, expired, cancelled or
revoked observations fail closed. A trusted, default-off stream-session startup
setting `observe_feedback_sources` enables the owner to request and validate
these observations against its current stream target, device generation and
capabilities. This setting does not bind pairing, modify installed grants or
open a feedback route. Ordinary unavailable responses are retried with a
bounded delay; unresolved broker work remains fail-closed. The runtime broker test uses a simulated lifecycle service; the stream-owner
workflow test uses a simulated input provider. These tests do not yet connect
the real lifecycle/NVHTTP/pairing providers.

The owner must independently verify the selected provider's supported feedback
schemas, negotiated client protocol and effective global/application/session/
client controller policy. An output callback, a raw client capability bit, or a
caller-supplied JSON object cannot establish that intersection. The observation
is point in time and must never itself open the feedback route.

A point-in-time observation is insufficient for slot reuse. Before changing a
bound device generation or retiring its lifecycle source, the input/lifecycle
owner must fence its local producers, cause the pairing/selected effect owner to
stop new source work, retire the whole selected control stream, and wait for the
matching pairing drain before admitting a replacement. If no coordinator or
receipt is available, the change remains fenced. Existing legacy delivery is
not automatically routed through this proposed barrier.

## Stream owner and retirement interface

`Record::control_retirement()` now returns only the currently owned control
stream handle, startup-selected transport service and validated 32-character
provider epoch from the actual control-open response. It returns `None` for
missing or malformed epochs or after the owned control resource is removed.
This is a provider-instance observation, not a retirement receipt or grant.

For an admitted feedback stream lease, the selected direct effect owner must:

1. Construct the exact `BindFeedbackSource` using the lifecycle-attested source,
   the input-provider boot epoch, the checked controller incarnation and the
   verified capability intersection. Use direct broker identity; the owner
   never supplies or derives the paired certificate.
2. Require NVHTTP's exact epoch/source/scope confirmation, then request and
   verify the exact `admit_stream_lease` result from pairing. An uncertain bind
   or admission is reconciled using its defined operation identity, and no
   feedback effect begins from an unresolved reply.
3. Route the authenticated source through the lease-aware stream queue, retain
   source acknowledgments, and poll/receive a pairing fence. On source, device,
   permission, provider, owner or host-configuration invalidation, close local
   admission and join all in-flight input/feedback workers.
4. Submit owner-scoped `retire` to the selected control service using the exact
   stream and provider epoch, and reconcile uncertainty using the same
   idempotent retirement operation identity. Only the
   authenticated selected broker provider's exact `whole_control_stream` /
   `retired` reply is a local receipt; JSON supplied by a client is not proof.
5. Complete and acknowledge the scoped pairing lease, then acknowledge the
   exact pairing fence generation. Lifecycle retirement and trust mutations may
   commit only after this barrier. Previously handed-off packets cannot be
   recalled; the control receipt promises no further Network SDK sends from
   that control stream, not remote delivery.

The test-gated scoped queue has one source item outstanding and uses the same
selected effect owner in process; it requires batching or real throughput
qualification for sustained 5 ms blocks. It is installed in the scoped
`pipeline/input.rs` path, while the legacy production path remains unchanged. The pairing authority admits one direct effect-owner
principal; composition must select stream-session or provide a reviewed
broker-mediated handshake, without passing Rust values across module boundaries.

## Missing proof before opening gates

A full real-provider test must join the real NVHTTP, lifecycle, pairing, input,
stream-session, control and Network providers through the runtime broker with
loopback-only sockets, then race controller-slot reuse, source teardown, input
and owner restart, permission revocation, host-configuration mutation, queued
ENet data, lost receipts and an in-flight local send. It must show that no
precommit trust mutation passes the barrier, stale incarnations never create an
effect, no local sends occur after the drain acknowledgment, and recovery does
not rely on timeouts. The lifecycle and host-configuration precommit fence and
independent review remain prerequisites, not workarounds for missing evidence.

## Current source-retirement checkpoint

The input `feedback_retirement_service` handshake waits after local cleanup for a
direct selected-owner response matching the binding, target, input boot epoch and
operation ID. The stream owner reserves each source before NVHTTP binding and
pairing admission, retains exact tickets and source identities, and closes the
whole transport incarnation to new reservations before draining it. The worker
fences all recorded sources, verifies the selected control provider's real
whole-stream retirement receipt, reconciles uncertain lease admission, completes
and acknowledges every admitted lease, and completes each source with the same
receipt before replying. It retains bounded completion tombstones for retries.
Missing or uncertain identities and receipts remain fenced. An authority status
watch handles a same-epoch global fence and acknowledges its drain generation
only after the registered sources and transport workers settle. A detected
source-specific fence retires its affected whole control transport.

The source admission, scoped feedback route and authority watch are enabled only
under unit-test qualification. The following evidence does not open the gate:

* A real broker, the actual pairing ledger and the selected control provider were
  tested together with two source records, an uncertain admission, a lost reply,
  queued ENet/retransmission work and the actual provider retirement receipt.
  Pairing's TLS/certificate pipeline and lifecycle provider were not in that test.
* A direct input-principal broker request was acknowledged after the same
  worker settled pairing and the real control provider. Cancellation and retry
  use the retained completion receipt. The test did not instantiate the real
  input module or lifecycle provider.
* Tests fence a pairing-ledger permission mutation before and during retirement,
  check a fresh authority epoch after the ledger drains, and check controller
  re-arrival is stopped before input-provider delivery. They do not prove the
  real pairing administration transaction, host-configuration precommit path,
  or a restarted stream-owner process.

A stream-owner restart currently loses the in-memory source registry and lease
operation identities. The existing lifecycle journal deliberately omits resource
handles, input binding tokens and source records; it cannot reconstruct the exact
pairing lookups or prove old control workers drained. Production binding must
remain closed until a reviewed durable recovery design and a real-provider test
cover that path. Feedback authority also does not authorize unrelated raw input
effects. The selected source route requires independent review, capability policy
coverage, and the full lifecycle/NVHTTP/pairing/input/stream/control integration
test described above before activation. The queue still needs throughput
qualification for sustained 5 ms blocks.

A possible authority-owned recovery contract is an effect-owner-only,
epoch-checked global restart fence followed by a bounded, stable snapshot of the
old owner's exact `{source, scope, lease_operation_id, sequence, lease_state}`
records. A completed but unacknowledged lease also needs a direct owner-only
lookup that can recover its ticket or otherwise acknowledge the exact completed
record; the current `lookup_stream_lease` returns `operation_complete` without a
ticket. The owner would group these records by the full transport incarnation,
retire each selected provider, reconcile every lease/source, acknowledge the
matching global drain, and recover into a fresh owner epoch. The enumeration
must not expose certificates or client identity. This is a proposed pairing API,
not an implemented or independently reviewed guarantee. A pairing-provider
restart additionally needs proof that old owner-scoped control transports have
been retired even when the old ledger itself is gone.
