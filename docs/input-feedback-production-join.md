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
or conflicting tagged records. That check does not remove already queued or
copied feedback, nor does it synchronously fence pairing on reuse. Those still
require the closed source-retirement barrier.

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
generation,capabilities}}`. It contains no certificate. The lifecycle source
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

## Current source-retirement integration mismatch

The committed source-retirement protocol requires an exact admitted source and
structured scope for every controller sharing a control transport. Input's raw
controller-arrival capabilities are client claims; the selected provider does
not expose an independently attested feedback schema intersection, and the
stream owner lacks an effective per-session controller-policy decision. Neither
value may be fabricated from the observation above. The input revoke path also
acknowledges local cleanup without a stream-owner retirement response; it has
no complete source/scope registry to issue a pairing fence.

The required bounded handoff is a direct, authenticated request from input to
the selected stream owner when an owned binding or device retires. The stream
owner must fence every exact admitted source on the shared control transport,
stop its local queue, request real control-provider retirement, reconcile the
provider receipt and complete and acknowledge every lease and source retirement
with that same receipt before replying. Input must hold its lifecycle revoke
acknowledgment until that reply. Missing or uncertain identities and receipts
remain fenced. The authority worker is repairing terminal lease replay,
controller-specific operation identities and shared-transport receipt agreement;
these must be covered with the combined real-provider test before activation.
