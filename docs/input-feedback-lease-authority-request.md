# Pairing authority request for scoped feedback leases

This is a proposed shared contract for the pairing owner, not an implemented
pairing request. Production source binding remains closed. The existing
`Admit` records a source and operation but does not retain the negotiated
capability set or stream handle/transport incarnation. It cannot establish the
scope required for a stream lease merely by putting a digest in an operation ID.

Proposed additive SDK types, subject to pairing/lifecycle owner review:

```text
StreamLeaseScope {
    source: effect_authority::Source,
    control_stream: u64,
    control_provider_epoch: String, // exactly 32 lowercase hex
    controller_slot: u16,
    capabilities: BTreeSet<input::extensions::Capability>, // 1..=32, ToClient
}
AdmitStream { epoch: String, operation_id: String, sequence: u64,
              scope: StreamLeaseScope }
```

The selected direct effect owner supplies this scope from the trusted,
authenticated protocol/lifecycle join and negotiated provider/adapter
intersection. Pairing must require its already bound exact `source`, selected
direct owner, matching epoch and globally increasing nonzero sequence. It must
retain the complete immutable scope with the admission and treat a retry with
any changed field as `operation_conflict`. An exact live retry returns the same
ticket, while after completion/acknowledgment an old sequence cannot be reused.
The existing 256-operation and source limits apply. Names/versions/directions
are schema identifiers, not client-wire negotiation tokens. The source/device
to controller-slot and stream-handle mapping is a trusted lifecycle/composition
assertion that must be verified by the owner; pairing cannot infer it from a
client string or prove a client's capability support on its own.

Proposed successful response:

```json
{"ok":true,"epoch":"<authority epoch>","operation_id":"<id>","ticket":"<ticket>","scope":{"source":"<exact source object>","control_stream":1,"control_provider_epoch":"<provider epoch>","controller_slot":7,"capabilities":[{"schema":"org.amaterasu.feedback.rumble","version":1,"direction":"to_client"}]}}
```

`source` in this example denotes the full structured `Source` object, not a
string. A stream-aware lookup after a lost response must return that exact scope,
ticket and existing `fenced` boolean. Repeating an admission is insufficient
once a fence has closed admission. `Complete` and `Acknowledge` can retain their
current request/reply shapes, provided a scoped record cannot be completed by a
mismatched operation/ticket/epoch. The SDK lease constructor must be updated to
accept and compare this returned scope, rather than independently supplied
fields, before the feature can be enabled.

When revocation begins, pairing fences new leases before its trust mutation and
must signal or expose `drain_required` to the selected owner. Until that owner
observes the fence, already admitted stream work may continue under its old
lease, and the mutation must remain uncommitted. Once observed, the owner stops
new local messages, joins in-flight calls, cancels or settles pending busy work,
retires the whole selected control stream, reconciles the exact provider receipt,
completes and acknowledges the aggregate ticket, and acknowledges the pairing
drain for the matching epoch/fence generation. Only then may pairing commit the
mutation. After commit, that lease cannot produce any new local Network SDK send;
packets already handed off cannot be recalled. Current status polling is not an
instant notification and is not a substitute for joining a blocked worker.

Recovery after authority, effect-owner, transport or lifecycle restart must
first retire/fence old actual workers and queues, prove their termination, and
then rotate/rebind epochs. Test lost admit/complete/drain replies, forged or
expanded capability sets, mismatched source/device/stream/provider generations,
owner restart, blocked native enqueue and in-flight/retransmitted ENet data.
Independent review and a combined authenticated broker/lifecycle/provider test
are required before opening the production binding.
