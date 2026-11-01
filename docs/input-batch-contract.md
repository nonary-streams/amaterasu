# Input batch contract

The versioned broker service `amaterasu.input/1` receives lease-scoped input
through `batch`. The service is selected by trusted startup composition; its
caller needs an explicit call grant. An active lease belongs to that caller and
its provider. Native modules are trusted code, and API grants do not sandbox
native code.

The request contains an opaque `lease` and an ordered, nonempty `events` array
of at most 64 events. Lease IDs are nonempty UTF-8 strings of at most 128 bytes
without control characters; consumers must not require a particular provider's
encoding. The shared Rust SDK exposes `InputRequest::Batch` and
`MAX_BATCH_EVENTS`. When the provider's active acquire or lookup reply includes
`"mixed_input_batches": true`, the lease accepts keyboard, pointer and
controller events in the same batch, preserving their order. This capability is
pinned for the lifetime of that lease. Omission or `false` means the caller
must retain the legacy separation between controller and other input events;
a non-boolean value makes the lease response invalid. Providers advertise the
capability with `InputBatchCapabilities` in the SDK. It is not inferred from a
provider identity or from a similarly sized native table.

A successful response contains `ok: true`, the same `lease`, and `accepted`
equal to the submitted event count. Suppressed events still count toward this
prefix. Acceptance means the selected provider accepted the operation locally;
it does not prove remote delivery.

If a controller event fails with `controller_unavailable` and the lease remains
usable, the provider returns `ok: false`, the same `lease`, `accepted` smaller
than the submitted count, and `partial_event_possible`. Events in the accepted
prefix are not resubmitted. The event immediately after that prefix may have
had an effect when `partial_event_possible` is true; the consumer discards that
uncertain controller event and may submit the untouched suffix. The current
stream consumer only recovers this controller error when that flag is `true`;
other values fail closed. This applies to
mixed batches only when the failed event is a controller event. Other errors,
malformed counts, a mismatched lease, and ambiguous replies do not prove that
the lease is still usable and retain the existing fail-and-cleanup behavior.
Lifecycle-bound batches keep their existing sequence and retirement rules; a
controller failure on that path fails closed rather than replaying an uncertain
operation.

`InputBatchCapabilities`, `lease_from_reply`, `batch_success`,
`record_batch_failure`, and `decode_batch_outcome` in
`amaterasu-module-sdk::input` expose these wire rules to providers and
consumers. A provider must preserve event order and its own bounded cancellation
and cleanup behavior. An equivalent service implementation can be selected
through composition without changing the consumer. The C SDK header
[`amaterasu_input.h`](../sdk/include/amaterasu_input.h) supplies the service,
capability and size constants for callers using the broker JSON API.
