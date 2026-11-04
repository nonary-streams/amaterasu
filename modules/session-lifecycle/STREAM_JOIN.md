# Shared stream sources

`prepare_stream` with `intent: "join"` creates an independent stream target in
an existing application session. Trusted startup policy must grant
`amaterasu.stream/join` to every selected lifecycle participant, and each
participant must implement the common SDK Join adapter after its usual
coordinator authentication. Unsupported participants reject admission before
provider effects. The coordinator freezes the original source context; request
payloads cannot supply `join_context`.

A Join retains the source's host policy, resource identity and geometry. Client
encoding size, rate, transport keys and input context remain independent. The
joining stream inherits the source provider settings while keeping its own
paired-client scope. Shared source mutation and reconfiguration are fenced while
the lease is retained.

Managed v2 providers receive the original finalized source output and exact
binding in `StreamJoinContext`. The inherited binding differs only in subscriber
generation. The existing source owner's native binding validation still gates
capture admission. The Join plan grants no native resource rights, mutation
rights, cleanup obligations or owner-control proposals; it does not allocate a
second native display borrow. The coordinator's durable subscription pins the
original borrow until all affected consumers retire.

External v1 or v2 providers use `capture_binding_mode: "external"`, with no
binding or binding owner. They retain their authenticated source host policy;
the selected capture/input providers keep their existing independent admission
contracts. Sharing an external native display remains the pipeline provider's
SDK responsibility.

Retirement distinguishes consumer shutdown from shared resource release. A
successful receipt has exact departing `target`, `consumer_retired`,
`resources_released`, `shared_retained`, and optional `shared_source`. Consumers
must use `stream_retirement_proof` with the expected target and frozen source.
Legacy successful receipts without these fields retain full-release semantics.
The last subscriber's receipt waits for original-source callbacks. Deferred or
unknown restoration retains recovery work and cannot certify resource release.

After restart, old epochs cannot prepare or activate streams. A selected capture
consumer must prove native retirement before the original release graph can run.
If the local journal lost the admission's target before Start, use SDK
`StreamPreparationLookup` on the existing lifecycle service:

```json
{"operation":"resolve_stream_preparation","session":{"kind":"session","host_epoch":"old-epoch","session_id":"session","generation":1},"preparation_operation_id":"rtsp-ticket-lifecycle-prepare","preparation_caller":"amaterasu.rtsp"}
```

This read-only operation requires an explicit `resolve_stream_preparation` grant,
the configured capture consumer identity and session delegation. The preparation
caller must own or delegate the same session. Responses echo the exact query
identity and provide `succeeded` with an admitted stream target and
`preparation_status`, or `pending`/`missing` with a null target. Resolution does
not certify successful preparation or resource retirement. `missing` is returned
only for a definitively absent old-epoch admission. Validate readback with
`validate_stream_preparation_resolution` before using its target.

Completed no-native Join history can be reclaimed only after exact callback,
consumer and unresolved-reference checks. Its operation IDs and stream scope
become durable replay fences. Capacity is reserved before admitting another
subscriber; exhausted or unresolved history fails closed without changing the
source lease.
