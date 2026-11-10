# Stacking and extension points

A module can build on another module without either one being rebuilt. There
are three ways to do it, and trusted policy decides which of them exist on a
given host:

| Mechanism | Use it to | The other module must | SDK |
| --- | --- | --- | --- |
| [Layer](#layers) | Change, watch or block the calls to an existing service. | Do nothing. | `requests::Relay`, `RuntimeClient::wrap` |
| [Hook](#hooks) | Take part in a decision the owner makes at a point that is not a service call. | Run the hook. | `hooks::HookRun`, `RuntimeClient::join_hook` |
| [Event](#events) | Follow what changed without affecting it. | Publish the event. | `events::Topic`, `events::Subscription` |

All three sit on the [control protocol](control-protocol.md) and use the same
request helpers as an ordinary provider or caller; read
[the control-plane SDK](control-plane-sdk.md) first. None of them carries
per-frame data: media uses [native interfaces](native-interfaces.md), which
are not layered.

Native modules are trusted code. Grants and layer listings decide what the
broker routes; they are not a sandbox.

## Layers

Policy lists the layers of a service, outermost first:

```json
{
  "principals": {
    "example.service-guard": { "call": ["example.notes/1"] }
  },
  "service_layers": {
    "example.notes/1": ["example.service-guard"]
  }
}
```

With that in place:

- A consumer's call to `example.notes/1` is delivered to the first layer.
- A layer's own call to the service is delivered to the next layer, and the
  last layer's call reaches the provider.
- The consumer and the provider keep using the one service name. Neither is
  rebuilt, and neither can tell from its own code that a layer is there.

The listing is the layer's authority to receive the calls; it needs no
`provide` grant, and the service's provider cannot also be one of its layers.
A layer needs the service's `call` grant to pass a request on.

```rust
use amaterasu_module_sdk::requests::Relay;

struct Guard {
    runtime: RuntimeClient,
    relay: Relay,
}

impl ManagedModule for Guard {
    fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
        runtime.wrap("example.notes/1")?;
        Ok(Self { runtime, relay: Relay::new() })
    }

    fn tick(&mut self) -> ModuleResult<()> {
        while let Some(request) = self.runtime.next_request()? {
            if request.caller == "example.untrusted" {
                request.refuse(&self.runtime, "permission_denied", "not admitted")?;
            } else {
                self.relay.forward(request)?;
            }
        }
        self.relay.poll(&self.runtime, |_request, reply| reply)?;
        Ok(())
    }
}
```

For each request a layer chooses one of four things:

- **Pass it on.** `relay.forward(request)` sends it to the next hop unchanged;
  `relay.forward_with(request, &payload)` sends something else in its place.
- **Change the reply.** The closure given to `relay.poll` sees each reply
  before the consumer does and returns what the consumer receives.
- **Answer it itself.** `request.reply(&runtime, &payload)`, as a provider
  would, without involving the next hop.
- **Refuse it.** `request.refuse(&runtime, code, message)`; see
  [refusing a request](#refusing-a-request).

`relay.poll_with` gives the layer every outcome of the inner call, not only a
reply, and lets it choose the answer. `Answer::relay(outcome)` is the
transparent choice: a reply passes through, a refusal is passed on with its
code, and an unknown outcome is left alone. Leave an unknown outcome alone: the
consumer's request then ends at its own deadline, which correctly tells it the
provider may have acted. Turning it into a refusal would tell the consumer
that nothing happened.

### Whose authority applies

A request a layer forwards keeps the caller of the request it received. The
provider therefore authorizes the consumer exactly as it would without the
layer, and state the provider binds to a caller stays bound to the consumer.
The broker takes that identity from its own record of the received request;
a layer cannot name an arbitrary caller. A call the layer makes for itself,
with no received request behind it, shows the layer.

This is why a layer is approved like a replacement provider: it can read,
change and answer every call to the service it wraps, as any of that
service's callers.

The forwarded call also cannot outlive the request it forwards, and it is
cancelled when that request ends.

### Approved API methods

A layer can wrap a service whose methods are installed API contracts. Use
`relay.forward_api(request)` or `forward_api_with(request, &arguments)`: the
method and version come from the delivered envelope. Policy must list the
layer among the method's approved `callers`. The broker checks the forwarded
arguments against the method's request schema, and checks the layer's reply to
the consumer against its response schema.

### Direct calls

A [direct call](direct-calls.md) follows the same stack. It enters the
outermost layer's `call_direct`. The layer can pass it on with
`runtime.call_direct(call.service, payload)`, and the next hop then sees the
consumer as its caller, as it does for a forwarded request.

A layer that returns `None` declines the call. The consumer then submits an
ordinary request, which arrives in the layer's `tick`. The provider is never
entered around a layer that declined.

Only a direct call made while serving a direct call keeps the consumer's
identity. Pass a queued request on with the relay: a direct call made from
`tick` shows the layer as its caller.

### When a layer is missing

- A package that lists a layer and omits its component fails verification.
- While any listed layer, or the provider, has not registered, calls to the
  service are refused at admission as `unavailable`. That is the code a
  consumer already receives while a provider starts.
- A layer is never skipped.

### Limits and cost

- At most 8 layers on one service and 64 layered services.
- Host-owned services and hook deliveries cannot be layered.
- A queued call through a layer takes extra ticks: the layer forwards it on
  one tick and relays the reply on a later one. Do not layer a service on a
  latency-sensitive path unless the layer serves its direct calls.
- A layer needs a host and a runtime built with layer support. An older host
  rejects a bootstrap policy that contains `service_layers`.

`sdk/examples/service-guard` is a complete layer with its component template
and policy fragment.

## Refusing a request

A provider or a layer ends a request without performing it:

```rust
request.refuse(&runtime, "permission_denied", "not during a stream")?;
```

The caller's `OutboundCall` settles as `CallOutcome::Refused`, the same
outcome it gets when the broker never admitted the request. `refusal.origin`
is `ErrorOrigin::Provider`, and `refusal.code` is the code given, so
`ErrorCode::PermissionDenied` or `ErrorCode::Busy` match whoever decided. A
code the service defines arrives as `ErrorCode::Other`.

Refuse only when the operation was not performed. After an effect, or when
the outcome is unknown, reply in the service's own terms or let the request
reach its deadline.

On the wire the caller reads the error code `refused.<code>`, which no code
the broker issues can be mistaken for. A runtime older than refusals answers
`refuse` with `invalid_request` and leaves the request live.

## Hooks

A hook is a named point where one module, the owner, asks other modules, the
participants, and then decides. Policy declares it:

```json
{
  "hooks": {
    "example.launch.environment/1": {
      "owner": "example.launcher",
      "participants": ["example.overlay", "example.mods"],
      "timeout_ms": 1000
    }
  },
  "principals": {
    "example.overlay": { "hook": ["example.launch.environment/1"] },
    "example.mods": { "hook": ["example.launch.environment/1"] }
  }
}
```

Only the owner can run the hook. Participants are asked one at a time, in the
listed order, and one deadline covers the whole run.

### The owner

```rust
use amaterasu_module_sdk::hooks::{HookOutcome, HookRun};
use amaterasu_module_sdk::requests::unique_operation_id;

let mut run = HookRun::new(
    "example.launch.environment/1",
    &unique_operation_id("launch.environment"),
    &environment,
)?
.best_effort()
.chained();

// Each tick, until it settles:
if let Poll::Ready(outcome) = run.poll(&runtime) {
    match outcome {
        HookOutcome::Completed(report) => { /* report.value(), report.answers() */ }
        HookOutcome::Refused(refusal) => { /* nobody was asked */ }
        HookOutcome::Stopped(stop) => { /* a required run ended early */ }
        HookOutcome::Unconfirmed(error) => { /* the result could not be read */ }
    }
}
```

Two choices shape a run:

| | Broadcast (default) | `chained()` |
| --- | --- | --- |
| **Required (default)** | Every participant must answer the owner's input. Use it for a decision that must not be made without them: any participant can stop it. | Each participant receives the previous one's result and returns the value to pass on. Any participant can stop it. |
| **`best_effort()`** | Every participant is asked and each outcome is reported. Use it for contributions the owner can do without. | A participant that gives no answer is passed over; the value it received goes to the next one. |

- A required run does not start while a participant is absent
  (`Refused` with `unavailable`). Once started, the first participant that
  refuses or fails ends it (`Stopped`), and the message names that
  participant.
- A best-effort run always completes. `report.failures()` lists the
  participants that gave no answer and why; an absent one reports
  `unavailable`.
- `report.value()` is the value a chain ends with: the last answer given, or
  the owner's input when nobody answered. `report.answers()` and
  `report.decode::<T>()` give each participant's answer.

The broker does not merge or check answers. The owner validates every
contribution, and the final value of a chain, before acting on it.

A hook that policy does not declare refuses the run with `not_found`. An owner
can therefore offer an extension point unconditionally and carry on with its
own behavior when nobody is composed into it.

Give each run its own operation identifier. A run that settles also gives the
identifier up, so an extension point can run as often as its owner needs.
`fenced()` keeps the identifier instead: a repeat then reports
`operation_expired` and never asks the participants again. The broker holds
512 fenced identifiers for its whole lifetime, so reserve it for effects that
must not repeat.

### A participant

```rust
fn start(runtime: RuntimeClient) -> ModuleResult<Self> {
    runtime.join_hook("example.launch.environment/1")?;
    Ok(Self { runtime })
}

fn tick(&mut self) -> ModuleResult<()> {
    while let Some(request) = self.runtime.next_request()? {
        if request.hook() == Some("example.launch.environment/1") {
            let environment = add_overlay(&request.payload);
            request.reply(&self.runtime, &environment)?;
        }
    }
    Ok(())
}
```

A run arrives like any other request, from the hook's owner, with
`request.hook()` set. Reply with the contribution, or in a chained run with
the value to pass on. `refuse` declines: that stops a required run and is
recorded against the participant in a best-effort one.

### Limits

- At most 32 participants on a hook, and 64 runs in progress at once.
- The hook's deadline is 1 to 30,000 ms for the whole run.
- The input, and every value passed along a chain, is at most 16 KiB. A
  chained value over the limit counts as that participant's failure.
- Chained runs and giving up an identifier need a runtime built with them.

## Events

An event tells subscribers that something changed. It is a notification, not
a delivery guarantee: publish a revision or an identifier and let subscribers
read the state from its owner.

```rust
use amaterasu_module_sdk::events::{Backlog, Delivery, Subscription, Topic};

pub const LIBRARY_CHANGED: Topic<LibraryChanged> = Topic::new("example.library.changed/1");

// The publisher, which holds the topic's `publish` grant:
runtime.publish(&LIBRARY_CHANGED, &LibraryChanged { revision })?;

// A subscriber, which holds its `subscribe` grant:
let mut changes = Subscription::open(&runtime, &LIBRARY_CHANGED, Backlog::Latest(1))?;
// each tick:
while let Some(delivery) = changes.next(&runtime)? {
    match delivery {
        Delivery::Event(event) => { /* event.publisher, event.sequence, event.payload */ }
        Delivery::Gap => { /* events were missed: read the current state */ }
        Delivery::Malformed { .. } => {}
    }
}
```

A `Topic` ties the name to the payload type, so the publisher and its
subscribers share one definition. The runtime stamps each event with the
publisher's identity and a per-publisher sequence number.

The `Backlog` says what a subscription keeps when events arrive faster than
they are read:

- `Backlog::Latest(n)` keeps the newest `n` and drops the oldest silently. Use
  it when only the latest state matters.
- `Backlog::Complete(n)` keeps every event up to `n`. When more arrive, the
  subscriber receives `Delivery::Gap` once, the subscription is renewed, and
  later events follow. Read the owner's current state after a gap.

A queue holds 1 to 64 events and the runtime allows 128 subscriptions.

The built-in topics are defined in `events::topics`:

| Topic | Published by | Payload |
| --- | --- | --- |
| `SESSION_CHANGED` (`amaterasu.session.changed/1`) | Session coordinator | `revision` |
| `CONFIGURATION_CHANGED` (`amaterasu.configuration.changed/1`) | Configuration | `owner`, `revision`, optional `kind` |

## Testing

The [module test kit](control-plane-sdk.md#testing-a-module) composes all of
this the way policy does on an installed host:

```rust
let host = TestHost::builder()
    .principal("example.notes", grants().provide("example.notes/1"))
    .principal("example.guard", grants().call("example.notes/1"))
    .principal("console", grants().call("example.notes/1"))
    .layers("example.notes/1", &["example.guard"])
    .extension_point("example.launch.environment/1", "example.launcher", &["example.overlay"])
    .build();
```

`settle` drives an `OutboundCall` and `settle_hook` drives a `HookRun`, ticking
the modules under test between polls. `crates/module-sdk/tests/stacking.rs`
exercises layers, hooks and events through the real runtime, and
`modules/session/src/module_tests.rs` stacks a layer on the unchanged session
module.
