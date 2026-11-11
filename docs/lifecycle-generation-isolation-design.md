# Lifecycle generation isolation

## Goal and scope

Ending a stream must immediately remove that stream's lifecycle state from the
admission decision for a later stream. This applies uniformly to every provider,
task, hook, transport, application and resource: an old cleanup failure, a
pending result, a lost reply or an exhausted retry budget must not by itself
keep a later generation in a closing or recovery state. Cleanup continues as
independent work, with its actual outcome visible. There is no special class of
cleanup whose failure can hold the entire lifecycle open.

This is a target architecture, not a description of current behavior. A fresh
generation can still fail for an actual present-day reason, such as unavailable
hardware, an occupied exclusive port, a full disk, or an unresponsive provider.
The design must isolate or replace old owners sufficiently that an old task
cannot itself monopolize a resource needed by the next generation. Where a
platform gives no way to isolate or reclaim an exclusive resource, the system
cannot honestly promise successful use of that resource under arbitrary failure;
it must report that specific resource failure rather than a global lifecycle
failure. A hung kernel or permanently unavailable hardware cannot be repaired by
a state-machine transition.

## Observable guarantee

For a qualified provider composition, once generation A's end transaction has
committed, a valid start request for generation B must reach actual streaming
within the composition's documented, finite startup deadline, provided the
required platform resources and client remain healthy and available. A's cleanup
may fail or remain pending forever. Accepting B, issuing a ticket, or moving it
to a different waiting state does not satisfy this guarantee. Qualification must
observe media received and decoded by the client, and verify that late work from
A cannot disrupt B after startup.

The deadline runs from receipt of B's valid start request and includes authority
initialization, containment, resource acquisition, and any provider replacement.
It must be configured before the attempt and cannot reset on retries or recovery
transitions. Tests must also bound A's end acknowledgment independently of its
cleanup, assuming functioning persistent storage and authority services; moving
the cleanup wait into the end transaction would violate the design.

Resources retained by A are part of the isolation obligation, not an automatic
exception to the guarantee. For failures within a provider's qualified failure
model, it must reclaim, replace, or independently allocate the required resource
within the startup budget. Renaming `cleanup_pending` to `resource_unavailable`
does not meet this requirement. Failures outside that model must identify the
specific resource, old owner when known, attempted recovery, and platform
limitation; they cannot be reported as successful isolation qualification.

## Current boundaries to replace

NVHTTP already retries failed cleanup and can mark it abandoned, freeing its
application admission (`modules/nvhttp/src/launch/cleanup_recovery.rs`). That
does not provide the property end to end: RTSP startup reconciliation and
ticket capacity retain broader barriers (`modules/rtsp/src/controller.rs` and
`modules/rtsp/README.md`), while lifecycle admissions and domain locks can
remain attached to uncertain provider work
(`modules/session-lifecycle/src/coordinator/failed_prepare_recovery.rs`).
Stream-session can also see startup reconciliation as pending. The existing
retirement evidence remains meaningful: freeing admission must not claim that
native resources were retired or erase their receipts.

## One model for all participants

Each stream attempt has a unique, durable generation identity. A generation
owns a set of resource leases and effect operations, and has two independent
state axes:

* **Admission:** `open` or `ended`. Ending atomically closes admission and
  revokes the generation's normal authority to create or mutate live resources.
  `ended` is irreversible, even if cleanup never finishes.
* **Retirement:** per-operation and per-resource `pending`, `confirmed`, or
  `unconfirmed`, with errors and retry metadata. This axis cannot change
  admission. Failed and unfinished work remains attributable to its generation.

An application retained for resume is still active by explicit policy, not
because retirement is pending. Ending an application closes all of its stream
generations; detaching one client closes only that client's generation. Shared
resources have explicit leases per owner, and are retired only after their last
valid owner releases them. The same rules apply to every lifecycle participant,
including extension providers and user commands.

The end transaction durably records the end decision, revocation and cleanup
intents before acknowledging the end. A crash before commit leaves the previous
decision recoverable; a crash after commit cannot reopen the generation. New
generation creation is an independent transaction, not a transition out of
the previous generation's retirement state. A storage failure is reported as
such; it must not be silently treated as a committed end.

## Effect authority and containment

Every effect request and result carries generation and operation identities.
The resource owner enforces authority at the actual mutation boundary, not only
when work is queued. End revokes the generation's normal authority, and late
results can update only the old retirement record. They cannot attach output,
change admission, release another generation's lease, or undo a newer effect.
Retries use the same durable operation identity and provider-specific status;
an unknown result is not proof of no effect. Cleanup commands are idempotent
or use exact resource identity and compare-and-delete semantics.

The SDK contract must define an ordering between revocation, mutation, and
successor ownership. Checking authority and then issuing an unguarded native
operation is insufficient: revocation can occur between those two actions.
For shared resources, the owner must serialize those decisions or use a fencing
mechanism enforced by the mutation destination. Work already in flight must
either finish before ownership transfers, remain confined to the old instance,
or have its executor contained before the successor receives that resource.
Waiting for an uncooperative operation to finish cannot be the only transfer
mechanism in a qualified composition. A lock around a potentially hanging native
call would merely move the admission barrier into the provider.

The durable end decision is authoritative. Each resource owner must enforce it
before accepting a later mutation or transferring ownership, including after a
crash; an asynchronous revocation notification alone is insufficient. End does
not wait for retirement. Any containment still needed for a particular resource
is charged to B's finite startup budget, without reopening A or blocking
unrelated resources.

Cleanup runs under separate supervisor authority scoped to A's exact resource
and operation identities. It may retire those resources after A's normal
authority is revoked, but cannot create new live resources, reopen A, release
B's leases, or restore shared state over a newer assignment. This authority
must survive restart and remains subject to current ownership checks. Ending A
must neither disable its cleanup nor grant the old worker general mutation
authority in order to finish it.

An in-process token cannot stop arbitrary native code or a command already
executing. Providers must therefore declare how effects are contained:

* Generation-owned resources use isolated handles or namespaces and may be
  quarantined with their old owner. A new generation gets a distinct instance.
* Shared resources use an owner that serializes changes and checks a versioned
  lease at the mutation boundary. An old restore cannot overwrite a newer
  assignment. Existing shared owners are not destroyed when one client ends.
* Uncooperative or potentially blocking work runs in a replaceable worker
  process with bounded queues and an independent retirement supervisor. The
  supervisor revokes access and contains the worker before allocating the
  corresponding shared resource to a successor. A process exit alone is not
  proof that an external side effect was undone.
* A resource that cannot support fencing, safe replacement or independent
  instances must expose that limitation explicitly. The scheduler can choose
  another instance or provider where available; otherwise it reports the
  exact unavailable resource. It must not invent retirement evidence.

This contract applies equally to built-in and replacement providers through
versioned SDK interfaces and trusted composition. Provider identity and native
handles stay inside the provider. A provider that cannot honor the contract
cannot silently participate in the guaranteed-isolation profile.

Each provider's qualification must document, for every resource it owns:

* The enforced mutation boundary, lease identity, and revocation ordering.
* How a successor obtains a usable resource when the old operation never
  returns, including the containment or replacement mechanism and time budget.
* How late effects, stale cleanup, and lost replies are prevented from changing
  successor resources, including effects delegated to external services or
  descendant processes.
* The supported failure model, finite capacity limits, and any platform failures
  for which independent allocation or safe reclamation is impossible.

Trusted composition must validate these capabilities for all selected providers,
including hooks and user commands. Arbitrary commands that retain unrestricted
access to shared state cannot acquire the guarantee merely by running in a child
process. Unsupported participants require an explicitly limited compatibility
profile. They must not silently weaken the guaranteed profile.

## Independent cleanup and bounded state

Cleanup is a durable, supervised queue keyed by generation and exact operation.
Each task has its own timeout, retry policy and observable terminal disposition;
dependencies are scoped to resources actually shared by those tasks. One
failed task does not stop unrelated cleanup or admission. Scheduler execution,
broker capacity, worker pools and result retention for cleanup must be isolated
from the capacity reserved for current-generation admission and streaming.
Do not execute blocking cleanup on the host tick.

Old records cannot consume an indefinitely growing in-memory slot, replay-fence
table, port allocation or active-session quota. Persist compact tombstones and
unresolved obligations separately from the bounded active set. Use unique,
non-reusable generation and operation identities so compaction does not make
late work authoritative again. Apply bounded disk retention only to records
whose safety obligations have actually been discharged or transferred to a
durable provider-owned containment record. If persistent storage itself is
exhausted, expose that concrete failure; deleting uncertain obligations is not
a safe capacity policy.

## Restart and migration

On restart, restore ended generations as ended and resume their cleanup in the
background. New admission waits only for initialization of the authority and
containment mechanisms needed for its selected resources, not for a global
reconciliation of every old stream. An epoch change fences old requests only
when the effect boundary actually enforces it; it does not establish native
retirement. Imported legacy records remain visible and quarantined with their
original identities. Never rewrite them as clean merely to unblock admission.

Implement incrementally across the full path: define the SDK contracts and
durable end/revocation transaction; implement provider fencing and containment;
separate RTSP, stream-session and lifecycle active capacity from retirement;
then remove global recovery gates only after each selected provider satisfies
the new contract. Migrate legacy state with explicit compatibility tests. Keep
the existing barriers for providers that have not yet acquired an equivalent
safe boundary; removing them early would allow old work to affect new streams.

## Acceptance tests

The first end-to-end acceptance case must leave A's cleanup permanently pending,
commit and acknowledge A's end within its independent bound, then start B through
the normal client launch path and receive and decode B's media within the startup
deadline. Keep A's cleanup pending throughout. Deliver A's delayed effects and
results after B starts and verify B continues streaming with its leases and
resources intact. Merely observing successful admission is a failing test.

Exercise an unchanged consumer with both the default and replacement providers.
At every lifecycle stage, inject a cleanup error, a permanently pending task,
a lost reply, and late success after a successor starts. End the old generation,
start a new one, and assert the old task cannot mutate or retire its resources.
Repeat through NVHTTP, RTSP, stream-session and the coordinator, including
application end, disconnect/resume and multiple clients sharing a resource.
Test restart at each persistence boundary, stale requests and results, saturated
cleanup queues, repeated failed generations, provider worker crashes, and
bounded active capacity. Check that unresolved retirement stays visible without
being reported as successful. Force revocation between authority checking and
native mutation, and exercise operations already in flight during transfer.
Verify that supervisor cleanup remains authorized for A's exact resources after
end, while attempts to mutate B or restore over B's assignments are rejected.
Retain an encoder, port, or display under A and prove the documented recovery
path supplies B with a usable resource within budget; a renamed resource error
does not pass. Repeat failed generations up to the documented capacity limits
to detect accumulation that a single replacement would hide.

Platform integration tests must separately prove the isolation and resource
reclamation behavior for actual display, audio, input, capture, encoder, network
and application providers; simulated broker tests alone do not establish the
guarantee on hardware.
