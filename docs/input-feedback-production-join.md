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
unique nor an authorization. The current additive response does **not** convey
an input boot epoch or lifecycle source label. `device_capabilities` is the
capability value observed at producer creation and can originate in the client's
controller-arrival packet; it is a claim, not proof of device, driver, client
protocol or policy support. The current legacy stream consumer ignores the added fields, so this observation
alone does not prevent an already queued or copied event from being sent after
reuse. That requires the still-closed source retirement barrier.

The selected stream owner still needs a direct input-provider, lease-bound
observation (or equivalent authenticated lifecycle callback) that supplies the
provider boot epoch, exact active lifecycle source/target and a current device
slot/generation in one bounded result. It must independently verify the selected
provider's supported feedback schemas, negotiated client protocol and effective
global/application/session/client controller policy. An output callback, a raw
client capability bit, or a caller-supplied JSON object cannot establish that
intersection. Current input acquisition happens before stream activation, so an
optional source observation must be refreshed after activation; existing
`feedback_peek` is not itself that refresh.

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
   stream and provider epoch, and reconcile a lost response with
   `retirement_status` and the same retirement operation ID. Only the
   authenticated selected broker provider's exact `whole_control_stream` /
   `retired` reply is a local receipt; JSON supplied by a client is not proof.
5. Complete and acknowledge the scoped pairing lease, then acknowledge the
   exact pairing fence generation. Lifecycle retirement and trust mutations may
   commit only after this barrier. Previously handed-off packets cannot be
   recalled; the control receipt promises no further Network SDK sends from
   that control stream, not remote delivery.

The current queue has one source item outstanding and uses the same selected
effect owner in process; it requires batching or real throughput qualification
for sustained 5 ms blocks. It is not installed into `pipeline/input.rs` or the
legacy `send_rumble` path. The pairing authority admits one direct effect-owner
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
