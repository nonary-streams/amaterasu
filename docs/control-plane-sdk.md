# Control-plane SDK

`amaterasu_module_sdk` wraps the [control protocol](control-protocol.md) for a
module that provides a broker service, calls another module's service, or
both. The wire commands remain available through `RuntimeClient::send`; the
helpers here carry the parts every module otherwise writes by hand: telling a
lost exchange from a refusal, replying to a request that already ended,
retrying an admission without running an operation twice, and releasing the
broker's record of a finished call.

These helpers cover queued requests. Media and other per-frame data use
[native interfaces](native-interfaces.md); a call that completes in
microseconds uses a [direct call](direct-calls.md). Native modules are trusted
code: grants decide what the broker routes, they are not a sandbox.

A module that builds on another one, by wrapping its service, taking part in
its hooks or following its events, uses the same helpers; see
[stacking and extension points](stacking-and-extension-points.md).

## A provider

```rust
use amaterasu_module_sdk::{ManagedModule, ModuleResult, RuntimeClient};
use serde::Deserialize;
use serde_json::json;

const SERVICE: &str = "example.counter/1";

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    step: u64,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Add {},
    Read {},
}

struct Counter {
    runtime: RuntimeClient,
    step: u64,
    total: u64,
}

impl ManagedModule for Counter {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        let settings: Settings = runtime.settings()?;
        runtime.provide(SERVICE)?;
        Ok(Self { runtime, step: settings.step, total: 0 })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        let Some(request) = self.runtime.next_request()? else {
            return Ok(());
        };
        let request = match request.parse::<Operation>() {
            Ok(request) => request,
            Err(malformed) => {
                let invalid = json!({"ok": false, "code": "invalid_request"});
                malformed.request.reply(&self.runtime, &invalid)?;
                return Ok(());
            }
        };
        if matches!(request.payload, Operation::Add {}) {
            self.total += self.step;
        }
        request.reply(&self.runtime, &json!({"ok": true, "total": self.total}))?;
        Ok(())
    }
}

amaterasu_module_sdk::export_module!(Counter);
```

- `settings` decodes the startup settings trusted composition selected for
  this module. A module without configured settings reads an empty object, so
  give every field a default.
- `provide` registers a service the module holds a `provide` grant for;
  `provide_api` registers one approved method of a versioned API.
- `next_request` returns one delivered request with the identities the broker
  attested: `caller` is the module that sent it and `initiator` is the
  principal the whole delegated chain acts for. Authorize from these fields,
  never from the payload.
- `parse` decodes the payload into the service's operation type. A payload
  that does not decode comes back untouched, so the provider answers it in its
  own service's terms.
- `reply` returns `ReplyDelivery::RequestEnded` when the caller cancelled, the
  deadline passed or the request's parent ended while the provider worked.
  That is ordinary and is not an error. Work the provider committed stands and
  the caller recovers it with an idempotent retry. A failed tick is reported
  to the host as a module failure, so do not turn an ended request into one.
- `request_active` (or `request.is_active`) checks, immediately before a side
  effect, that the request can still take a reply. It is an observation, not a
  lock.
- `refuse` (or `request.refuse`) ends a request the provider did not perform,
  with a short code and one line of text. The caller's call settles as
  `Refused`, so it knows a new call is safe. `malformed.refuse(&runtime)`
  declines an undecodable payload as `invalid_request`. Refuse only when
  nothing happened; after an effect, reply in the service's own terms.

Reply on a later tick by keeping `request.id` and calling
`runtime.reply(id, &payload)` when the work finishes. Never block in `tick`.

## A caller

A queued request takes several exchanges over several ticks. `OutboundCall`
owns the sequence:

```rust
use amaterasu_module_sdk::requests::{CallOutcome, OutboundCall, RequestOptions};
use std::task::Poll;

let mut call = OutboundCall::request(
    RequestOptions {
        service: "example.counter/1",
        timeout_ms: 5_000,
        operation_id: "example.console.add.1",
        parent: None,
    },
    &json!({"action": "add"}),
)?;

// Each tick, until it settles:
if let Poll::Ready(outcome) = call.poll(&runtime) {
    match outcome.decode::<Total>() {
        CallOutcome::Completed(total) => { /* the provider's reply */ }
        CallOutcome::Refused(refusal) => { /* the operation was not performed */ }
        CallOutcome::Unconfirmed(error) => { /* the provider may have acted */ }
    }
}
```

| Outcome | What is known | Broker record |
| --- | --- | --- |
| `Completed` | The provider replied. | Released. |
| `Refused` | The operation was not performed. Either the broker never admitted the request (no grant, no provider, invalid input, a full table, cancellation before admission), or the provider or a layer in front of it declined it with `refuse`. Starting a new call is safe. | None was created, or released. |
| `Unconfirmed` | The request was or may have been admitted and ended without a usable reply: deadline, cancellation, repeated transport failure, or a reply of the wrong shape. | Kept for a mutating call, released for a read-only or single-use one. |

The operation identifier is the retry identity. The broker returns the
original request for an identical submission while it retains the operation,
so a lost admission is repeated safely and a kept record stops the same
identifier and payload from running twice. Use `unique_operation_id(prefix)`
for a call that is never retried as the same logical operation, and a fixed
identifier when a later attempt must find the earlier one. A refusal with
`ErrorCode::Conflict` means the identifier is already bound to different
input.

Declare a call `read_only()` when the provider changes no state for it. A
read-only call that ends without a reply releases its record at once.
Declare it `single_use()` when its operation identifier will never be
submitted again: no repeat can need the record, so it is released the same
way, and the outcome is still `Unconfirmed`.

`cancel()` asks for the request to end; keep polling. A reply the provider
recorded first is still returned. A call whose admission is unknown is looked
up rather than resubmitted, so cancelling never starts work.

Set `parent` to the identifier of the request being served to make a delegated
call. Its authority and deadline are narrowed to the parent's, and it is
cancelled when the parent ends.

A call that is dropped before it settles leaves its request with the broker
until the deadline and the five-minute retention pass. The broker retains at
most 512 requests, so cancel and keep polling instead of dropping.

## Errors

The helpers return `ControlResult<T>`. `?` converts it into a `ModuleResult`,
and a module that needs to decide matches on the type:

| `ControlError` | Meaning |
| --- | --- |
| `Transport` | The command or its reply was lost. The broker may have acted. |
| `Broker(BrokerError)` | The reply was an error; `code` is an `ErrorCode` and `origin` says whether the broker or the serving module decided it. |
| `UnexpectedReply` | The broker answered with a reply the operation does not define. |
| `Payload` | A payload could not be encoded or decoded. |

`ErrorCode` names the codes the runtime produces. A code a provider defines
for its own service arrives as `ErrorCode::Other`. Use `error.is(&code)` to
test for a code the broker itself decided; it never matches a provider's
refusal, so a provider that refuses with `not_found` is not mistaken for a
handle the broker no longer holds. `error.refusal()` returns the provider's
refusal when that is what the error is.

## Testing a module

`amaterasu-module-testkit` runs a module inside an ordinary `cargo test`.

`TestHost` is the runtime the installed host loads, created in the test
process. Grants are enforced, requests are routed and deduplicated, deadlines
expire and direct calls reach their provider:

```rust
use amaterasu_module_testkit::{TestHost, grants, settle};

let host = TestHost::builder()
    .principal("example.counter", grants().provide("example.counter/1"))
    .principal("console", grants().call("example.counter/1"))
    .settings("example.counter", json!({"step": 5}))
    .build();
let mut counter = host.load::<Counter>("example.counter");
let console = host.client("console");

let outcome = settle(&console, &mut call, || counter.tick_times(1));
assert_eq!(outcome, CallOutcome::Completed(json!({"ok": true, "total": 5})));
```

`load` starts the module through the lifecycle table it exports, including
panic containment and direct-call registration. `client` returns a
`RuntimeClient` for a principal, to start a module directly when a test needs
its internals, or to play one of its peers by hand with the same helpers.

`ScriptedHost` answers every command from a function the test supplies. Use it
for what a healthy runtime will not do on request: `lose_replies(n)` loses the
next replies in transport, and the script can return an error at one exact
step. It replaces a hand-written `extern "C"` endpoint.

Neither loads a signed package, verifies a manifest, or starts a worker
process. Platform effects and hardware behavior need their own tests.

The builder also stacks layers on a service and declares hooks, and
`settle_hook` drives a hook run; see
[stacking and extension points](stacking-and-extension-points.md#testing).

`crates/module-sdk/tests/control_plane.rs` is a complete provider and its
tests. The session, foreground-context and updater modules use these helpers.

## Not covered yet

Session-lifecycle participation is hand-written by each provider against the
types in `extension_api`; see [the session extension API](session-extension-api.md).
No built-in module runs a hook yet, and the default profiles compose no
layers: both exist once an operator's policy declares them.
