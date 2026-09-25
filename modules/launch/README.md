# Managed library launch

`amaterasu-launch` provides `amaterasu.launch/1`. It resolves a catalog executable,
creates an owned coordination session, acquires an exclusive application lease,
launches the process through the application provider, and activates the session.
An active record means a managed application and coordination session exist;
it does not imply capture, encoding, media transport, input delivery, or streaming
readiness.

Requests are strict JSON:

```json
{"action":"launch","operation_id":"living-room-1","app_id":"app-0000000000000001"}
{"action":"launch_export","operation_id":"moonlight-1","app_id":"app-0000000000000001","global_preparation":[]}
{"action":"lookup","operation_id":"moonlight-1"}
{"action":"status","launch_id":1}
{"action":"stop","launch_id":1}
{"action":"list","offset":0,"limit":16}
```

Callers cannot override the executable, arguments, environment, working directory,
process ID, session ID, lease resource, or catalog owner. A host settings bridge
may include a `global_preparation` snapshot on either launch action; omitted means
an empty list. The snapshot uses the same structured command format and validation
as catalog preparation.
Catalog entries with `launch.kind:"executable"` use the application provider's
native process path. Fixed-owner Playnite entries use the typed `launch.kind:"playnite"`
target and a validated Playnite GUID; launch sends that ID to the Playnite extension
and never turns it into an executable or URI. Bundle/URI `application` entries,
detached launchers, and streaming startup remain unsupported. A catalog executable
may be an absolute native path or a bare command resolved through `PATH`; relative
paths containing a directory component are rejected. Lookup uses absolute `PATH`
entries only, is capped at 128 entries and 16 KiB, and on Windows considers `.com`
and `.exe` for extensionless names. Implicit `.bat` and `.cmd` executables remain
rejected. Executable and working-directory values support bounded `$()` references
to configured launch environment values, then host environment values; an unset
reference expands to empty, malformed references fail admission, and `$$` produces
a literal dollar. Matching outer single or double quotes are trimmed from these two
path fields. Expansion does not interpret shell syntax and does not apply to
arguments, detached commands, preparation commands, or Steam metadata. Resolved
program/cwd paths must fit application limits (1024 bytes); arguments remain limited
to 64 values, 1024 bytes each, and 8192 total bytes. A catalog entry valid as
cross-platform metadata may still be unsupported on this host. The application
provider enforces its remaining native launch constraints, including rejection of
implicit batch executables.

A preparation plan contains at most eight ordered `run` commands with optional
paired `undo` commands. Global steps are prepended to catalog steps unless the
resolved catalog entry has `exclude_global_prep_cmd:true` (missing means false);
that flag skips only global steps. The legacy spelling
`exclude-global-prep-cmd` is accepted as an alias, but supplying both spellings
is invalid. The combined plan also has an eight-step maximum and exceeds that
limit with `preparation_limit_exceeded` before acquiring a session lease. Each
command uses literal argv, a cleared explicit
environment, an optional native working directory, and a 1–30000 ms timeout
(10000 ms by default). Command paths, arguments, and environments use the same
native application bounds. Launch snapshots the supplied global list in the
idempotency record, parses and validates the resolved catalog plan, and forwards
the merged plan to the application provider. Reusing an operation ID with a
different global list returns `operation_conflict`. It applies process defaults
only where the catalog has no per-application value (`auto_detach:true`,
`wait_all:true`, `exit_timeout_seconds:10`). Typed Playnite targets also default
to a 10-second exit timeout. For Playnite and Playnite Fullscreen, global
preparation must finish before the provider starts. Cleanup confirms the provider
has stopped before running reverse-order undo, and retains the session lease if
undo fails so the caller can retry. Executable launches merge global steps before
catalog steps, and owe reverse-order undo for every run command they started.

This payload does not implement Vibeshine's raw `do`/`undo` command strings or
per-command `elevated`. A settings bridge must map configured commands to this
explicit executable/argv model; these semantics remain a parity gap. Application
launch arguments, detached commands, and preparation command fields also do not
receive Vibeshine's general `$()` expansion.

Typed `playnite_fullscreen` catalog targets carry only a bounded exit timeout
(default 10 seconds, maximum 300); they contain no request-controlled path or
game ID. Launch delegates `fullscreen_start`, then polls the returned opaque
ticket and activates the coordination session only after the provider reports
`running`. Stop and ambiguous-start recovery use the same ticket/operation ID;
the lease remains held until the provider confirms cleanup. A zero fullscreen
timeout posts a safe close request and uses the provider's fixed 20-second
recovery grace, rather than waiting indefinitely.

Catalog lookup uses `resolve_launch` with the original runtime parent. Catalog
accepts this action only from configured launch principals and selects the
immutable runtime initiating principal's namespace. No supplied owner string is
accepted. Records belong to the launch request's immediate runtime caller;
underlying sessions and processes belong to this launch provider. The lease is
scoped to the initiating catalog principal and app ID, so separate operations
cannot concurrently launch that same entry while a prior process owns its lease.
Different catalog entries or namespaces have separate resources. Knowledge of a
returned process/session ID does not grant callers direct access through those
providers.

`launch_export` keeps the same parent delegation but asks catalog for
`resolve_exported_launch`. Catalog requires this provider in `launch_principals`,
maps the immutable initiating principal through its startup `export_principals`,
and returns the canonical mapped owner with the entry. Launch accepts no owner,
path, arguments, working directory, or environment from this request. It retains
the canonical owner privately and derives the same owner-and-app lease identity
as an ordinary management launch, so the two paths cannot run the same catalog
entry concurrently. A missing mapping or foreign initiating caller fails before
session creation or process launch.

Startup settings are optional and default to an empty environment:

```json
{"environment":{"SystemRoot":"C:\\Windows"}}
```

The application environment is cleared, then populated with this trusted startup
map. Host variables are not inherited implicitly. Supply OS/application-required
variables explicitly; Windows Python and some native applications may require
`SystemRoot`. Keys/values use application limits (32 keys, 128-byte keys containing
ASCII letters/digits/underscore, 1024-byte values, 4096 total bytes, no NUL or
case-insensitive duplicate keys). No request-level environment override exists.

The module needs a provide grant for `amaterasu.launch/1` and call grants for
`amaterasu.catalog/1`, `amaterasu.state/1`, `amaterasu.session/1`,
`amaterasu.application/1`, and `amaterasu.playnite/1`. Catalog settings must include
this module principal in `launch_principals`. Playnite-owned entries are writable only
by the fixed Playnite importer. Launch callers also need these downstream grants under the
runtime's parent-delegation rules. Cleanup deliberately uses parentless requests
under the launch provider's own grants so cancellation of the originating request
cannot block teardown. All native providers are trusted code; grants are not a
native-code sandbox.

Accepted workflows and status/stop replies return:

```json
{"ok":true,"launch":{"launch_id":1,"operation_id":"living-room-1","app_id":"app-0000000000000001","state":"active","session_id":1,"process_id":1,"failure":null,"cleanup_error":null}}
```

Application catalog entries may set `config_overrides.amd_av1_latency_mode` to
exactly `auto`, `none`, `power_saving`, `realtime`, or `lowest` (case-sensitive).
Missing or null values inherit; explicit `auto` remains present in the application
snapshot. Launch preserves the catalog value in launch and status records without
resolving global or client precedence.

`ok:true` acknowledges a retained lifecycle record, not necessarily a successful
launch. Inspect `state`: `starting`, `active`, `stopping`, `stopped`, `cancelled`,
`failed`, or `cleanup_failed`. A normal launch completes its response after
activation; startup failures complete only after rollback or a reported cleanup
failure. IDs are nullable until obtained. `failure` records the startup/cancellation
cause; `cleanup_error` identifies unresolved teardown. Error details are fixed
codes, not echoed downstream messages, arguments, or environment values.

Operation IDs are 1–128 ASCII graphic bytes and unique per immediate caller for
this module instance. Repeating the same operation, app, launch action, and
resolved catalog principal returns the original record without launching again,
even after it stops. A different app, direct/exported action, or catalog principal
yields `operation_conflict`. Exported retries must also retain the original
immutable runtime initiator, even when two initiators map to the same catalog
owner. Catalog edits
do not change an already accepted operation. A new operation ID intentionally
creates a new launch. At most 32 records total and 16 per caller are retained;
terminal records are not evicted. Lists contain up to 16 caller-owned records
and `next_offset`. Foreign/missing IDs both return `launch_not_found`.
`lookup {operation_id}` follows the same ownership rule and returns the retained
record, allowing a caller to reconcile a lost launch response without scanning.

Stop first moves the coordination session toward stopping, terminates the owned
application tree, and polls while the application reports `stopping` and runs its
reverse-order undo. Only application states `exited`, `launch_failed`, and
`cancelled` are terminal; only then does launch finish the session and release its
leases. If termination or application undo cannot be confirmed, the session lease
remains held and the record reports `cleanup_failed`. Application
`cleanup_failed` includes the intended terminal outcome and retains pending undo;
an explicit launch `stop` retries it and preserves that outcome. The provider
attempts process termination even when session stop fails, and requires the final
session response to confirm stopped state and no remaining leases. It never claims
the resource is free solely because an OS kill was requested.

Launch waits through application `preparing` and does not activate the coordination
session until the application reports `running`. Preparation failure and natural
process exit can report `stopping` while undo runs; launch keeps the lease and polls
until the terminal outcome is known. Monitoring rotates among active launches and
gets a turn after at most eight received requests, so continuous status/list traffic
cannot indefinitely starve it. One dependency workflow runs at a time; dependency
requests have at most five-second deadlines, normal startup respects the caller's
capped 30-second deadline, and rollback has a separate 20-second budget. Long-running
dependencies can delay unrelated requests within those bounds. Cleanup failures
retain ownership metadata and require stop retry; they are not silently discarded.

A cancelled or lost dependency result may hide an already committed session
or process. Before reconciling an unexpected or failed result fetch, the module
cancels the known broker request and releases its retained entry. This prevents a
still-queued spawn from executing after a lookup reports it missing. If either
operation cannot be confirmed, the record retains that request ID, reports
`cleanup_failed`, and holds its lease until an explicit stop retry settles the
request. At most one unresolved request is retained per bounded launch record.
Cancellation cannot undo a mutation already delivered to its provider; lookup
still relies on the session/application providers' serialized processing.
The module marks submitted mutations and reconciles them with
caller-owned session-key/process-operation lookup, never by replaying create
or spawn during rollback. Active-request checks run before normal stages, and
failed delivery of the initial activation response also starts rollback. A
cancelled retry of an already active operation does not cancel that original
launch. State and provider mutation replies are validated against expected
operation/session identities before adopting IDs. Consumed internal runtime
results use the SDK's ephemeral-result release helper.

For a typed Playnite launch, launch acquires the same catalog-app session lease,
then waits for the Playnite provider's opaque ticket to report `running` before
activating. Stop waits for a matching extension `gameStopped` event before it finishes
the session. If the start acknowledgement is lost, launch looks up the stable internal
operation ID and never replays `launch_start`. If the Playnite provider lost its
in-memory ticket after a reload, launch asks it to reconnect and issue a bounded stop;
the extension's launcher connection receives an active-game sync for a running
matching GUID. Missing or malformed status, disconnect, and stop timeout retain the
lease as `cleanup_failed`, where an explicit stop retries recovery. The Playnite
cleanup deadline allows for two authenticated pipe connections and status polling,
with a 321-second maximum for a 300-second provider timeout. This slice does not
implement the reference helper's focus policy or process watchdog verification.

Launch records, idempotency, and reconciliation are in memory. They assume the
catalog/session/application providers retain their instance state while this
provider runs. Independent provider reloads, host crashes, and durable restart
reconciliation are unsupported. Dropping this provider alone cannot drive
asynchronous teardown; the embedding host must perform orderly stop before
unloading, or unload the application provider whose own Drop performs best-effort
OS cleanup. Unix hard host death can bypass process-group cleanup; Windows Job
Objects have their separately documented host-death behavior. Session state
alone is never proof that a process stopped.

Tests run actual controlled subprocesses through a deterministic transport
harness modeling session/application contracts. They cover idempotency, foreign
ownership, ordered teardown, exclusive lease contention, natural exits under
continuous ingress, cancellation after committed mutations, lost transport
results before and after spawn, cancellation/release failure with retained leases,
cancellation at activation completion, canonical preparation forwarding,
preparing/stopping waits, cleanup-failed outcome retention and retry,
exported-owner lease contention and lookup, and retry after a termination failure.
Typed Playnite tests cover fixed-owner target validation, explicit preflight proof
of no start, lost acknowledgement reconciliation, provider-module status loss,
missing ticket recovery, and bounded stop timeout. Full dynamic-provider/HTTP
integration is tested separately by the host.
