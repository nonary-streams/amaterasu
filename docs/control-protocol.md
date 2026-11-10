# Draft control protocol, version 1

Native entry points and function-table layouts are declared in `crates/abi` and
`sdk/include/amaterasu.h`. Rust types must not cross the C boundary. Reply buffers
are borrowed for one callback and copied by the caller. The shell destroys feature
modules before the runtime, and libraries stay loaded through their destructors.
Module lifecycle calls are serialized in the current development host. The control
broker uses a mutex and releases it before invoking the reply callback. Optional
[native service bindings](native-interfaces.md) call providers outside runtime locks;
the product interface defines its own threading and buffer-lifetime requirements.

Rust modules normally use the typed helpers in the
[control-plane SDK](control-plane-sdk.md) instead of building these envelopes.

## Operations

Every input is an internally tagged JSON object using `op`. Unknown fields in
control envelopes are rejected. Service/topic/hook names include an interface
major, for example `example.echo/1`. Current grants match exact strings; there are
no wildcards or caller-supplied identity fields.

| Operation | Authority and result |
| --- | --- |
| `get_settings` | Returns this endpoint's startup settings; other principals' settings are inaccessible |
| `register` | Requires `provide`; a service has one selected provider |
| `native_register` | Unsafe trusted-provider operation; requires separate `native_provide` grant |
| `native_bind` | Requires separate `native_use`; binds a versioned table to the authenticated module principal |
| `native_release` | Only the binding owner can release its native interface |
| `request` | Requires `call`; queues work and returns a request ID |
| `poll` | Delivers the next request belonging to this provider |
| `complete` | Only the provider of a delivered, live request can complete it |
| `check_request` | Provider checks that its delivered request is still active before a native side effect; this is not a lock against later cancellation |
| `take_result` | Only the requester can read the result |
| `release_result` | Only the requester can discard a completed/cancelled result and its retry record |
| `cancel` | Only the requester can cancel its request |
| `subscribe` | Requires `subscribe`; allocates a bounded queue |
| `publish` | Requires `publish`; runtime supplies publisher identity and sequence |
| `next_event` / `unsubscribe` | Only the subscription owner can use its handle |
| `register_hook` | Requires a hook grant and inclusion in its participant list |
| `run_hook` | Only the configured hook owner can initiate it |
| `hook_result` | Owner advances participants in configured order and collects proposals |

Requests include a deadline, operation ID, service, and payload. Delegated calls
include a parent request ID; the provider must have received that live request.
Effective service grants are intersected with the parent's authority and its
absolute deadline. Ending a parent cancels unfinished delegated work.

Matching retries reuse a request ID; reusing an operation ID with different input
is rejected. Results and deduplication records are retained for five minutes after
completion. This is bounded in-memory deduplication, not durable exactly-once
execution. Domain mutations will need persistent operation records where required.

`release_result` explicitly relinquishes both the result and its operation-ID
deduplication record before expiry. It rejects unfinished requests; cancel first
if abandoning work. Reusing the operation ID after release starts new work, never
reuses the opaque request handle. Do not release a call whose retry protection is
still needed. Internal pipelines with unique operation IDs use the SDK's
`send_ephemeral` to copy terminal results and release their tracking. Ordinary
`send` and console requests keep the default retention behavior. This avoids
filling the 512-record budget with already consumed internal HTTP/service calls.

## Queue and timeout behavior

- Requests have a deadline of 1–30,000 milliseconds and at most 16 KiB of payload.
- The runtime retains at most 512 requests; capacity exhaustion is explicit.
- There are at most 128 subscriptions, each with capacity 1–64 events.
- Event sequences are per publisher/topic, not globally ordered.
- A full coalescing queue drops its oldest entry. Other queues become desynchronized,
  discard their backlog, and report `resync_required` until unsubscribed.
- Recovery requires a new subscription and a revisioned snapshot from the domain
  owner. Snapshot services are not implemented in the fixture.
- Up to 64 hook invocations can run. One absolute deadline covers the complete
  ordered invocation. Missing participants and timed-out hooks fail closed.
- Hook proposals are not automatically merged. The domain owner must validate them
  and resolve or reject conflicts. Optional fallback policy belongs to that owner.

These conservative development limits need workload validation before production.
Requests, events, and hooks have Rust envelope types; generic domain payload schema
registration, generated TypeScript types, per-principal quotas, hook cancellation,
and interface adapters remain unfinished. Domain payloads are currently opaque
JSON with a size bound. Media transport must use dedicated interfaces, not this
control queue.

The configuration library separately validates registered boolean/integer/string
settings and persists them through the state library. This does not yet constitute
a general runtime service-payload schema registry. Product service response pages
are bounded to the runtime's 16 KiB limit, including JSON escaping and metadata.
