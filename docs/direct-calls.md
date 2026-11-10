# Direct calls

A direct call lets a module serve one request of its broker service
synchronously, on the caller's thread, instead of through a queued request.

## Why

A queued request is right for work that takes time. The caller submits it, the
provider picks it up on its next tick, and the caller reads the result on a
later one. Each hand-off waits for a host pass, and the caller keeps a pending
request with its own cancellation and uncertainty rules.

That round trip is the entire cost of an operation that finishes in
microseconds. Delivering one input batch took about three host passes: a mean of
4.7 ms and up to 36 ms on a measured stream, for a few device writes. A direct
call does the same work before the call returns.

## Semantics

- **Same service, same payload.** A direct call carries the request the service
  already defines and returns the reply it already defines. It adds no method
  and no schema.
- **Same authority.** The runtime checks the caller's `call` grant and routes
  only to the provider that registered the service. The provider receives the
  runtime-authenticated caller, as it does for a request.
- **Declining is always allowed.** The provider may decline any call. The caller
  then submits the identical payload as an ordinary request. A declined call
  must leave no effect behind, so the caller can never run an operation twice.
- **One thread.** A handler runs only on the thread that makes its module's
  lifecycle calls. A call from any other thread is declined before the module is
  entered, so a module needs no new synchronization.
- **No reentry.** A call that arrives while the module is already on the stack
  is declined instead of aliasing its state.

| Reply to the caller | Meaning |
| --- | --- |
| `Completed(payload)` | The provider handled the call; this is its reply. |
| `Unavailable` | The provider was not entered, or declined. Nothing happened. Submit a request. |
| error | The provider was entered and failed. The outcome is unknown, as for a failed request. |

`Unavailable` covers an older runtime, a provider built without direct support,
a missing grant, the wrong thread and a declined operation. The caller treats
them all the same way.

## Serving

Implement `ManagedModule::call_direct`. Return `None` to decline.

```rust
fn call_direct(&mut self, call: DirectCall<'_>) -> Option<serde_json::Value> {
    if call.service != SERVICE || call.payload["action"] != "batch" {
        return None;
    }
    // Decline before any effect when the work cannot finish in this call.
    if self.work_is_waiting_for_a_tick() {
        return None;
    }
    Some(self.deliver(call.caller, call.payload))
}
```

Serve directly only what completes within the call. Anything that needs another
module, a later tick or a durable write belongs to the request path, where its
jobs and cleanup rules already live. Decline whenever a request of the same
service is already waiting, so a direct call cannot overtake it.

A module that can take a call and then not know how it ended implements
`ManagedModule::serve_direct` instead, which adds a third outcome:
`DirectService::Failed` tells the caller the call failed without claiming that
nothing happened. A [layer](stacking-and-extension-points.md#direct-calls)
that passes a direct call on needs it; `call_direct` is enough otherwise.

`export_module!` exports the handler as `amaterasu_module_direct_v1`. The host
reads it from the verified library and registers it with the runtime. No module
passes a function address through a control message, so serving direct calls
needs no native grant and no policy change.

## Calling

```rust
match runtime.call_direct(INPUT_SERVICE, payload.clone())? {
    DirectReply::Completed(reply) => accept(reply),
    DirectReply::Unavailable { .. } => submit_as_request(payload),
}
```

Write the consumer once. It must work against a provider that serves nothing
directly, because a replacement provider may not.

## Input batches

The input provider serves a batch directly when it can complete it at once:

| Batch | Served directly |
| --- | --- |
| Unbound batch on a live lease | yes |
| Lifecycle-bound pointer, controller, pen or touch batch on an active binding | yes |
| Lifecycle-bound keyboard batch while the cached shortcut snapshot holds no approved bindings | yes |
| Keyboard batch with no cached snapshot yet, or one that may match a shortcut | no: needs the shortcut job |
| Binding not yet validated, or revoked | no: needs the lifecycle coordinator |
| Any request already waiting for a tick, or shutdown announced | no: keeps queue order |

A declined bound batch has not consumed its lifecycle sequence number, so the
queued request carries the same `operation_id`.

The stream session offers each batch directly right after it reads control
messages, in the same tick. It never offers one while a queued batch of the same
stream is outstanding. After three consecutive declines it stops offering for a
second, so a provider without direct support costs one extra call per second
rather than one per batch. Streams with managed lifecycle capture keep the
queued path, which admits control work one validated permit at a time.

## Contract and tests

`ModuleDirectApi` and `RuntimeDirectApi` are defined in `amaterasu-abi` and in
[`amaterasu.h`](../sdk/include/amaterasu.h). `call_direct` is the control
message `{"op":"call_direct","service":...,"payload":...}`; the
`direct_unavailable` error code proves that the provider did nothing.

- `crates/runtime/src/ffi/tests/direct_and_trace.rs`: one consumer served by a
  provider with and without direct support; grants, thread confinement and
  removal.
- `crates/module-sdk/src/exports/tests.rs`: reentry, another thread, a declined
  operation and a panicking handler.
- `modules/input/src/concurrent_module_tests/direct.rs`: the real input router,
  including sequence handling shared by both transports.
- `modules/stream-session/src/pipeline/tests/input.rs`: same-tick delivery,
  fallback, ordering across transports and failure handling.
