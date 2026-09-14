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
{"action":"launch_export","operation_id":"moonlight-1","app_id":"app-0000000000000001"}
{"action":"lookup","operation_id":"moonlight-1"}
{"action":"status","launch_id":1}
{"action":"stop","launch_id":1}
{"action":"list","offset":0,"limit":16}
```

Callers cannot override the executable, arguments, environment, working directory,
process ID, session ID, lease resource, or catalog owner. Only catalog entries
with `launch.kind:"executable"` are supported. The application provider receives
the catalog program, argument array, and optional working directory. Bundle/URI
`application` entries, command preparation/undo, detached launchers, and streaming
startup remain unsupported. Paths must be absolute on the current native host
and fit application limits (1024-byte program/cwd, 64 arguments, 1024 bytes each,
8192 total argument bytes); a catalog entry valid as cross-platform metadata may
still be unsupported on this host. The application provider enforces its remaining
native launch constraints, including rejection of implicit batch executables.

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
`amaterasu.catalog/1`, `amaterasu.state/1`, `amaterasu.session/1`, and
`amaterasu.application/1`. Catalog settings must include this module principal in
`launch_principals`. Launch callers also need these downstream grants under the
runtime's parent-delegation rules. Cleanup deliberately uses parentless requests
under the launch provider's own grants so cancellation of the originating request
cannot block teardown. All native providers are trusted code; grants are not a
native-code sandbox.

Accepted workflows and status/stop replies return:

```json
{"ok":true,"launch":{"launch_id":1,"operation_id":"living-room-1","app_id":"app-0000000000000001","state":"active","session_id":1,"process_id":1,"failure":null,"cleanup_error":null}}
```

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
application tree, then finishes the session and releases leases. If termination
cannot be confirmed, the session lease remains held and the record reports
`cleanup_failed`. Retrying stop repeats safe owned cleanup and can resolve a
transient failure. The provider attempts process termination even when session
stop fails, and requires the final session response to confirm stopped state
and no remaining leases. It never claims the resource is free solely because
an OS kill was requested.

Natural process exits trigger the same session cleanup. Monitoring rotates among
active launches and gets a turn after at most eight received requests, so
continuous status/list traffic cannot indefinitely starve it. One dependency
workflow runs at a time; dependency requests have at most five-second deadlines,
normal startup respects the caller's capped 30-second deadline, and rollback has
a separate 20-second budget. Long-running dependencies can delay unrelated
requests within those bounds. Cleanup failures retain ownership metadata and
require stop retry; they are not silently discarded.

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
cancellation at activation completion, exported-owner lease contention and
lookup, and retry after a termination failure. Full dynamic-provider/HTTP
integration is tested separately by the host.
