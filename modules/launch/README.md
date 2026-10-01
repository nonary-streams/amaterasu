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
paths containing a directory component are rejected. Lookup preserves `PATH`
order and resolves relative or empty entries against the expanded explicit app
working directory, or the host's current directory when none is configured. It
retains an absolute executable path before launch so later cwd changes cannot
select another executable. Lookup is capped at 128 entries and 16 KiB, and on Windows considers `.com`
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

A preparation plan contains at most eight ordered steps. Structured steps use
`run` commands with optional paired `undo` commands. Raw compatibility steps
use `do`, `undo`, and `elevated` fields, retaining the strings as supplied.
Launch accepts the structured host snapshot as `global_preparation` and the
raw host snapshot as `legacy_global_preparation`; both default to empty. The
resolved plan orders structured global steps, raw global steps, structured
application steps, then raw application steps. The raw and structured global
snapshots are both retained for retries, and changing either under the same
operation ID returns `operation_conflict`. Global steps are omitted when the
resolved catalog entry has `exclude_global_prep_cmd:true` (missing means false);
that flag skips both global forms. The legacy spelling
`exclude-global-prep-cmd` is accepted as an alias, but supplying both spellings
is invalid. The combined plan also has an eight-step maximum and exceeds that
limit with `preparation_limit_exceeded` before acquiring a session lease. Each
structured command uses literal argv, a cleared explicit
environment, an optional native working directory, and a 1–30000 ms timeout
(10000 ms by default). Command paths, arguments, and environments use the same
native application bounds. Launch snapshots the supplied global lists in the
idempotency record, parses and validates the resolved catalog plan, and forwards
the merged plan to the application provider. It applies process defaults
only where the catalog has no per-application value (`auto_detach:true`,
`wait_all:true`, `exit_timeout_seconds:10`). Typed Playnite targets also default
to a 10-second exit timeout. For Playnite and Playnite Fullscreen, global
preparation must finish before the provider starts. Cleanup confirms the provider
has stopped before running reverse-order undo, and retains the session lease if
undo fails so the caller can retry. Executable launches merge global steps before
catalog steps, and owe reverse-order undo for every run command they started.
Prepare-only provider leases receive the target's snapshotted exit timeout, so
raw undo uses `max(exit_timeout_seconds, 15)` seconds for both executable and
Playnite targets. Structured undo keeps its explicit millisecond timeout.
Raw commands are forwarded as opaque command text with their elevation flag;
catalog and launch do not expand or shell-parse their strings. The application
provider expands `$(NAME)` and `$$` only when it executes a raw preparation step.
Application launch arguments, detached commands, and structured preparation
commands do not receive Vibeshine's general `$()` expansion. For executable,
Playnite, and Playnite Fullscreen entries, launch forwards an explicitly
configured catalog `working_directory` as the preparation cwd. A cwd inferred
from the main executable is not treated as an explicit app directory, so raw
commands without one infer their own target directory from the resolved
executable or `PATH`.
Executable launches also forward this explicit directory separately for detached
commands. When no app directory is configured, the application provider infers
each detached target's own directory, even when the main executable has an
inferred cwd. Typed per-command cwd overrides retain precedence.
The catalog's optional top-level `output` is command-log policy. At catalog
resolution, launch expands bounded `$(NAME)` references and `$$` using the same
retained launch environment/path parser. Empty text or the exact string `null`
selects null output. A relative filename resolves against the captured host cwd,
independently of an app's working directory. The resolved absolute path is stored
on the launch record before any session or child mutation and forwarded to the
application or provider preparation lease. Preflight callbacks, catalog edits,
changed environment, and operation retries do not re-resolve the retained path.
The application owns one append-open handle through reverse undo and failed
cleanup retries; typed detached stream paths retain precedence. Provider output
without commands still opens an output-only preparation lease, so its file
open/support failure prevents provider start. External Playnite game stdout and
stderr remain outside the current provider ABI. Output text is never forwarded
in API reports. The reference's configuration-parse expansion timing and native
Windows SYSTEM output behavior remain qualification gaps.

Typed `playnite_fullscreen` catalog targets carry only a bounded exit timeout
(default 10 seconds, maximum 300); they contain no request-controlled path or
game ID. Launch delegates `fullscreen_start`, then polls the returned opaque
ticket and activates the coordination session only after the provider reports
`running`. Stop and ambiguous-start recovery use the same ticket/operation ID;
the lease remains held until the provider confirms cleanup. A zero fullscreen
timeout posts a safe close request and uses the provider's fixed 20-second
recovery grace, rather than waiting indefinitely.

Typed `desktop` catalog targets are process-less sessions. They carry no program,
path, or provider identity, only an optional working directory, an exit timeout
(default 10, maximum 300) and the merged preparation. Launch reuses the prepare-only
application lease described for Playnite, without a provider start: it creates the
session, acquires the owner-and-app exclusive lease, sends `prepare` with the global
and application plan (global steps first, honoring `exclude_global_prep_cmd`), polls
`preparation_status`, and activates the session only when the lease reports `ready`.
An entry with no steps and no `output` skips the application provider and activates
directly. Launch, status, lookup and list report `process_id:null` and omit
`executable_path`, so the frame-limiter snapshot verifier never accepts a Desktop
launch and frame-limiter application is skipped. `preflight_export`, the preflight
token, frozen `config_overrides`, idempotent `operation_id`, `operation_conflict`
and `launch_export` work as for other kinds. `cleanup_budget_ms` is frozen at 20,000
ms plus each configured undo's budget and three seconds of containment.

Stop, cancellation and client quit run `preparation_stop`, which undoes in reverse
order and confirms the lease complete, then stop and finish the session so the
exclusive lease is released. A failed undo, an unconfirmed preparation, or a lost
`prepare` acknowledgement retains the lease: the ambiguous prepare is never
replayed, and cleanup uses the stable preparation operation ID. A failed undo
reports `cleanup_failed` and an explicit `stop` retries it. An active Desktop is
not polled, because nothing can exit on its own; it ends only by stop,
cancellation, or client quit. Real Moonlight launch of Desktop, and capture or
encode of a live KDE output, remain pending real-client and native qualification.

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

`nvenc_spatial_aq` is read as a strict optional boolean: absent or null inherits,
while explicit `false` remains present in the launch snapshot. Wrong JSON types
reject the catalog response. Launch, status, and same-operation retries retain the
value captured from the catalog revision used to start the application. Launch
does not resolve global or client precedence or project this value into nested
encode settings; L02 supplies route-aware resolution and projection.
When a catalog entry has application overrides, launch and status responses also
include a sparse `config_overrides` object: `fec_percentage`, `max_bitrate`,
`minimum_fps_target`, `video_max_batch_size_kb`, `stream_audio`, `audio_sink`,
`virtual_sink`, `audio_sink_capture_only`, `back_button_timeout`, `controller`,
`gamepad`, `keyboard`, `mouse`, `frame_limiter_enable`,
`frame_limiter_fps_limit`, `frame_limiter_provider`,
`frame_limiter_disable_vsync`, `frame_limiter_auto_virtual_framegen`,
`rtss_frame_limit_type`, and `ds4_back_as_touchpad_click`. Missing values remain absent; explicit zero,
`false` values, and empty audio selectors are preserved from the catalog revision
used to start the application. `minimum_fps_target` must be finite and from 0
through 1000, and `video_max_batch_size_kb` must be from 0 through 64. The
frame limiter FPS limit accepts 0 through 1000 with millihertz precision. Its
provider is platform-specific (`auto|rtss|nvidia-control-panel|none` on Windows,
`auto|mangohud|proton|mangohud-proton|none` on Linux, and `auto` on macOS).
Auto virtual frame generation is normalized to `enabled|vrr|legacy|disabled`;
legacy booleans are accepted on input. RTSS frame limit type is Windows-only.
The launch record snapshots these settings from the catalog revision used to
start the application. Consumer behavior for frame limiter and DS4 settings is
provided by their runtime modules.

Exported launches can call `preflight_export` with the same `operation_id`,
`app_id`, and global preparation lists used by the later `launch_export`. The
authenticated runtime initiator selects the exported Catalog owner. Preflight
resolves and validates one Catalog row without creating a session or starting an
application. It returns a short-lived opaque `preflight_token`,
`catalog_revision`, sparse `config_overrides`, and the executable path when the
selected target is executable. `launch_export` supplies that token plus the
effective `frame_limiter_settings` and sparse
`client_frame_limiter_overrides`. Launch validates the snapshot against the
frozen application and client policy, then starts the target captured during
preflight. Later Catalog edits cannot change that in-flight launch. The launch
record, status, lookup, and identical retries retain the effective snapshot,
Catalog revision, sparse overrides, and executable path. Tokenless legacy
`launch_export` requests remain supported when no effective snapshot is supplied.

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
capped 30-second deadline, and rollback has a separate frozen cleanup budget. Long-running
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
with a 321-second provider-stop budget for a 300-second provider timeout. The
overall cleanup deadline additionally includes every configured undo's execution
budget and three seconds per undo for containment and polling. Executable
cleanup similarly includes the application exit timeout and ordered undo;
individual dependency RPC deadlines remain at most five seconds. The overall
deadline is set once per cleanup pass and retained during polling.
The merged resolved plan also freezes `cleanup_budget_ms` before effects. Preflight,
status, lookup, and retained terminal records report the same 20,000..2,745,000 ms
scalar even after target ownership is released. NVHTTP consumes that scalar for
its outer cleanup deadline, so a long configured ordered undo does not encounter
a fixed 20-second session deadline. While cleanup runs, each tick admits at most
one owner-authorized status/lookup/list readback before advancing the active
cleanup work. An owned stop for that active record acknowledges `stopping`
without restarting undo or extending its deadline. Other mutations return
`workflow_busy`. Readback transport failure retains the pending cleanup request,
and list pages obey the 16,384-byte response bound as well as their count limit.
A timeout retains ownership and the session lease for an explicit retry. This slice does not
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

Sparse software encoder overrides support exact `sw_preset`/`sw_tune` strings
(up to 128/256 UTF-8 bytes, no controls), `encoder: "auto" | "software"`, and
a requested `min_threads` integer from 1 through 256 (default 2). Null/absence
inherit; empty tuning strings remain explicit. The actual x264 backend validates
codec vocabulary when opening software and may cap the effective thread count
based on frame size or runtime limits. NVHTTP resolves each field client before
application before global and freezes the launch snapshot.
See [software policy](../nvhttp/README.md#software-encoder-policy) for runtime
availability, source differences and remaining qualification gates.
