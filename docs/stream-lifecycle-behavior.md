# Stream lifecycle ownership and recovery

Amaterasu treats transport, application, capture, input, audio, display and
preparation as separate lifetimes. A successful request submission, expired
request, changed host epoch or empty local list does not prove a native effect
has retired. Consumers use the selected versioned provider contracts and retain
unresolved ownership until those contracts establish retirement.

Retaining ownership is not the same as holding admission. A record keeps its
cleanup identity and what it still owes for as long as it exists, and reports
that honestly. It stops counting against any limit, slot or lock the moment
its owner is asked to end. `lifecycle-admission-and-retirement.md` states that
rule and where each layer applies it.

## Shutdown order

A consumer closes admission, fences startup, and drains owned work while its
providers continue receiving ticks. Trusted package dependencies determine when
a provider may begin its own quiescence. An unrelated failing component must
not suppress progress in an independent branch. Each shutdown budget remains
bounded; polling must not extend a cleanup deadline or automatically replay an
arbitrary failed undo command.

Before consumer drain, the host announces its remaining budget through the
optional versioned `ModuleShutdownApi`. The SDK exposes that same contract to
external modules as `ManagedModule::prepare_shutdown`. Announcement closes new
admission and stores the earliest deadline; it does not begin provider teardown.
Application stop, active and future undo commands, and destructor fallback share
that deadline. The standard limits leave 20 seconds for module drain, 25 seconds
for worker exit and 30 seconds before the service manager intervenes. Legacy
modules without the optional announcement retain their older contract; the host
cannot infer that their destructors implement the deadline.

Direct SDK callers need the same shutdown guarantees as Moonlight callers.
Launch, RTSP and stream-session therefore own cooperative cleanup themselves;
NVHTTP is not the sole owner of their shutdown progress. Destructors are a final
bounded fallback and cannot substitute for cooperative provider progress.

A cancellation before submission must not submit a new effect merely to cancel
it. After submission becomes uncertain, cleanup retains the exact operation and
request identity, settles that request, and obtains authoritative lookup or
retirement evidence. Cancelling a hook also cancels its delegated request tree
before the hook's participant request is released.

## Stream and application lifetimes

An ordinary disconnect releases the departing transport and its input. A
resumable application retains its application policy and any shared source
still owned by another consumer. An explicit application end drains all related
transports before application cleanup; a configured stream-end lifetime policy
can request that same end behavior. A joined transport never acquires the
application owner's cleanup authority.

Quit can arrive after disconnect has already started transport cleanup. It
upgrades that cleanup to end the application within the existing deadline;
late transport status replies cannot downgrade Quit back to pause.

Capture and input retirement precede topology restoration. Pending display
removal remains pending until the provider observes disappearance. A safely
parked display may release active stream admission while retaining its durable
native recovery record and reporting the actual parked output.
The same removal guard applies when an intermediate setup layout fails to
restore: an observed surviving output permits unplugging the idle display,
while all unconfirmed snapshots remain retained for recovery.

## Preparation and restart

Reached preparation steps own their undo commands, including steps with an
empty `do` and nonempty `undo`. Unreached steps own no undo. Cleanup visits owed
undo commands in reverse order, preserving failed or uncertain effects for an
explicit retry.

A lifecycle preparation that finishes after disconnect still belongs to its
original operation. RTSP resolves and validates that preparation; stream-session
persists its cleanup identity before fencing or retirement, without starting
native media merely to stop it.

Restart recovery uses the original caller, target, generation, frozen provider
graph and provider evidence. Releasing one reference cannot remove a resource
still leased by another owner. Persisted virtual-display references are not
retired merely because the manager restarted; global topology restoration must
wait for the recovered reference cohort to release.

Audio absence proof is scoped to the original Pulse server instance and exact
native module. A missing sink alone does not prove an uncertain unload completed.
New identity-bearing intents require a matching server identity, complete native
module observation and an unchanged durable intent before absence can retire
ownership. Legacy intents without that evidence remain conservative. Older
strict readers cannot consume a pending identity-bearing intent; an empty sink
map remains compatible.

## Behavioral reference and regression coverage

The comparison uses Vibeshine commit
`43cf27252a205221a295126a5fcd9ad87ba9c914` as a behavioral reference. Its source is
not copied or translated. The useful invariants are ownership and ordering;
Vibeshine's best-effort continuation after an undo error is not native retirement
proof for Amaterasu.

| Behavior | Vibeshine reference | Amaterasu regression location |
| --- | --- | --- |
| Roll back owned launch work after cancellation | `src/process.cpp`, `proc_t::terminate`; `src/nvhttp.cpp` launch rollback | `modules/launch/src/pipeline/tests.rs` and `tests/shutdown.rs` |
| Drain startup completion before dropping RTSP work | `src/rtsp.cpp`, startup-pool shutdown | `modules/rtsp/src/controller/ownership_tests.rs` and `tests.rs` |
| Preserve application policy on pause and shared-client detach | `src/stream.cpp`, session owner accounting and join; frame-limiter suspend | `modules/nvhttp/src/launch/lifecycle_tests.rs` and `shared_session_tests.rs` |
| Reset input before media drain | `src/stream.cpp`, request_stop and join | `modules/stream-session/src/pipeline/tests/input.rs` |
| Keep a shared native source until its last consumer releases | `src/stream.cpp`, shared runtime ownership | `modules/stream-session/src/pipeline/tests/shared_capture.rs`; lifecycle coordinator regression tests |
| Wait for native display retirement or preserve parked ownership | Linux private-display recovery and virtual-display cleanup policy | `modules/stream-session/src/pipeline/tests/virtual_display.rs`; virtual-display worker tests |
| Run reached undo hooks in reverse order | `src/process.cpp`, launch commands and termination | `modules/application/src/lifecycle.rs` tests and `tests.rs` |
| Continue independent shutdown progress while preserving dependencies | `src/main.cpp`, `src/rtsp.cpp`, application termination | `crates/host/src/loading/teardown_tests.rs` |
| Request graceful worker stop before forced containment | `tools/sunshinesvc.cpp`, user-session worker termination | `crates/host/src/worker_shutdown.rs`; Windows supervisor source review |
| Reconcile exact native resources after restart | Linux private-display recovery; Windows startup display recovery | Stream-session restart tests, lifecycle recovery tests, audio journal tests and virtual-display worker tests |

## Remaining recovery boundaries

Launch currently advertises `durable_recovery: false`. A new launch runtime
cannot certify that a previous runtime's application process and preparation
undo commands retired. The fixed retirement SDK deliberately reports this as
unconfirmed; epoch fencing is not a replacement for durable native recovery.
Historical session cleanup can proceed only when independently verified
application retirement and the required exact child retirement evidence exist.
Unretired managed capture and input from an older runtime require their own
supported native recovery evidence.

The Windows interactive supervisor now requests EOF-driven worker cleanup and
requires successful retirement before replacement. Timeout, failed cleanup and
nonzero worker exit remain errors; forced termination is final containment.
Windows compilation, SCM timing and native crash/restart qualification remain
separate from portable deterministic tests. Linux fixture success does not
establish Windows process, desktop, audio or real-client playback behavior.
None of these source comparisons authorizes editing recovery journals or
interrupting an active stream.
