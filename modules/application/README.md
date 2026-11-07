# Managed applications

`amaterasu-application` builds a `cdylib` and `rlib` serving
`amaterasu.application/1`. It launches actual OS processes with `std::process`,
owns process trees, and polls exits on each module tick. The module
requires a `provide` grant for this service; callers require its
`call` grant. Caller identity comes from the runtime, never the payload.
Grant this service only to trusted administrative modules: executable selection
is arbitrary code execution under the host OS account. Native modules are trusted
code; grants are not a native-code sandbox. This service has no public HTTP route.

Requests are strict JSON objects. Unknown fields and unknown actions are rejected.
For example, on a Unix host:

```json
{"action":"launch","operation_id":"maintenance-1","executable":"/usr/bin/true","args":[],"environment":{},"cwd":"/tmp"}
{"action":"status","process_id":1}
{"action":"terminate","process_id":1}
{"action":"list","offset":0,"limit":16}
```

Launch policy also accepts `auto_detach` (default `true`), `wait_all` (default
`true`), `exit_timeout_seconds` (default 10, maximum 300), and an ordered
`detached` list of up to 16 command strings or typed command objects. `wait_all` keeps the app active while
any process in its contained group remains after the root exits. `auto_detach`
keeps a successful root that exits within five seconds as a status-driven app,
after `wait_all` is satisfied. A zero exit timeout skips graceful shutdown.
Detached command strings use platform argv quoting, run best-effort before the
primary executable, and are not attached to its process group or termination
lifecycle. They inherit the app's explicit environment and working directory.
On Linux, `proton_dualsense_compatibility` defaults to `false`; when enabled on a
Steam-ID launch, the primary process receives
`PROTON_KEEP_SONY_AUDIO_ENDPOINT_VISIBLE=1` and
`PROTON_SONY_WINDOWS_DEVICE_NAMES=1` only for keys absent from the explicit
launch environment. Explicit values, including empty strings, take precedence.
This sets the launched process environment; it does not inject variables into an
already-running Steam broker. URI handoff needs a separate stream-owned hook.
For an enabled Linux Steam-ID launch, the application service starts an
app-ID-scoped, same-UID authenticated hook service before handing off to Steam.
It adds a marked block to supported Proton `user_settings.py` files under the
discovered Steam roots. The block is inert when no matching stream is active,
sets only these two environment defaults, and preserves explicit values from
Proton user settings and the environment Proton receives, including empty
strings. It does not forward arbitrary per-request process variables through
an already-running Steam broker.
The service reference is held with the application record and stops after
launch cleanup settles; the marked block can remain installed without changing
later launches. Symlinks, hard links, non-user-owned files, oversized settings
files, and modified Amaterasu markers are refused rather than replaced. If a
Proton installation is discovered but none accepts the hook, the Steam handoff
fails before it is started. On systems with no discovered Proton installation,
native Steam launches can proceed and the service remains available for the
stream lifetime.
The optional launch field `detached_cwd` separates that policy from the primary
process cwd: omission retains inherited cwd for existing direct callers, null
infers each detached target's directory, and an absolute path selects a common
directory. A typed child's `cwd` takes precedence. Catalog launches forward the
explicit app directory or null, preserving independent target directories when
the main process cwd was inferred. Directory policy changes conflict with an
existing operation ID rather than starting additional detached processes.
Before spawning, bare targets resolve against the merged command environment's
`PATH`, falling back to the host's `PATH` only when none is supplied. Relative
or empty entries use the command's inherited or overridden cwd, or the host's
current directory when no cwd is supplied. Without an explicit cwd, the resolved
target's directory becomes its cwd. Lookup preserves entry order, skips
non-executable Unix files, and is capped at 128 entries and 16 KiB. A lookup
failure remains best-effort and does not prevent later commands or the primary
application from starting. Child environment inheritance remains explicit.
The typed form accepts `{"command":"...","environment":{"NAME":"value"},"cwd":"/absolute/path","stdout":"/absolute/out.log","stderr":"/absolute/err.log","elevated":false}`;
`working_directory` is an alias for `cwd`. The per-command environment overlays
the launch environment for that detached process only; its cwd overrides the
launch cwd. The primary process and other detached commands keep their own
settings. Both forms may appear in one list. Before argv parsing, `$(NAME)`
expands from the command override, then the launch environment, then the
host process environment; `$$` produces a literal dollar sign, and absent names
produce an empty string. Other dollar text remains literal, and an unclosed
`$(...)` reference is rejected before any preparation step starts. Expansion is
bounded and is not shell evaluation:
the expanded string still goes through the host platform's argv parser and
existing count, length, and argument limits. All overrides are validated before
any command starts. Optional stdout/stderr paths append child output without a
shell; omitted streams remain null. Each detached command opens only its own
files; an explicit stream path overrides the shared application output for that
stream. Omitted streams inherit shared output when configured, and otherwise
remain null. A Windows SYSTEM host
rejects redirected launches with `detached_output_unsupported` before any child
starts. Its `CreateProcessAsUserW` path cannot safely pass output handles across
sessions, and temporary inheritable handles could leak to concurrent host
spawns. Supporting that host requires a broker running as the selected user.
Failure to obtain the active-user token returns `detached_output_unavailable`.
A file-open or detached-command spawn failure remains best-effort and does not
fail the main launch. A typed command's optional `elevated` boolean overrides
the launch-level flag for that detached child alone. Legacy strings and typed
commands without the field inherit the launch flag. On a Windows SYSTEM host,
`true` selects the active user's linked elevated token only when one exists;
standard users keep their original token, and a missing active session fails
the child launch without falling back to SYSTEM. Non-SYSTEM Windows and Unix
hosts retain their current identity, so the flag does not elevate them.

Launch may also contain up to eight ordered preparation steps:

```json
{
  "action": "launch",
  "operation_id": "prepared-1",
  "executable": "/opt/game/bin/game",
  "preparation": [
    {
      "run": {"executable": "/opt/tools/display-mode", "args": ["streaming"], "environment": {}, "timeout_ms": 10000},
      "undo": {"executable": "/opt/tools/display-mode", "args": ["desktop"], "environment": {}, "timeout_ms": 10000}
    }
  ]
}
```

Preparation supports both the structured `run`/`undo` command form above and the
raw compatibility form used by catalog launchers:

```json
{"action":"launch","operation_id":"legacy-prepared-1","executable":"/opt/game/bin/game","environment":{"PROFILE":"streaming"},"preparation_cwd":"/opt/game","preparation":[{"do":"configure --profile=$(PROFILE)","undo":"configure --profile=desktop","elevated":true}]}
```

Structured commands use literal argv, a cleared explicit environment, optional
absolute `cwd`, and the application output destination, just like the primary
application. They have
no `elevated` field and are spawned with `elevated` set to false. Raw steps
accept exact `do` and `undo` command strings (both default empty) and an
`elevated` flag (default false). Raw strings are split with the host platform's
argument parser without invoking a shell. `$(NAME)` expands from the launch's
explicit environment map, then the module process environment; `$$` yields a
literal dollar sign. Raw commands overlay the explicit launch environment onto
the inherited environment. The launch coordinator supplies `preparation_cwd`
only when the catalog explicitly configures an application `working_directory`;
otherwise each raw target's directory is inferred from its resolved path or
`PATH`. This is separate from `cwd`, which may be inferred from the main
executable when the catalog omits `working_directory`.
Raw preparation uses the same bounded executable lookup as detached commands.
Explicit relative target paths resolve against the configured preparation cwd
or the current host directory. Both raw preparation and detached commands reject
implicit `.bat`/`.cmd` targets; configure an interpreter explicitly instead.
Raw `do` waits for the command to exit, matching the reference helper; an
explicit stop or cancellation still begins bounded process-tree cleanup. Raw
`undo` snapshots a deadline of `max(exit_timeout_seconds, 15)` seconds, matching
the reference's undo wait budget. Prepare-only leases accept the same
`exit_timeout_seconds` policy as launch (default 10, range 0 through 300);
changing that policy under an existing operation ID conflicts. Structured
commands retain their explicitly configured 1–30,000 ms deadlines. Preparation
shares the application output handles. Windows URL and associated-file launches
are not resolved through shell associations. The reference detaches an undo that
exceeds its deadline; this module retains its contained tree and waits for it to
settle before attempting an older undo, keeping ownership available for cleanup
retry. Cleanup attempts each owed undo once per pass in reverse order and
continues after spawn errors,
nonzero exits, and timeouts whose commands have been contained and reaped. It
retains each failed obligation and reports all failures from the pass; successful
undos are removed. If a wait or containment error leaves an undo child unsettled,
cleanup retains that child and defers older undos until an explicit retry can
observe it again. Status calls do not retry failed undos. An explicit terminate
request starts a new cleanup pass for the remaining obligations. The reference
also tolerates a permission-denied preparation spawn error for an application
without a primary command; this module reports the spawn failure.

Steps run one at a time. A successfully spawned `run` owes its paired `undo` even
if it later exits unsuccessfully, times out, or is cancelled, because it may have
changed external state. Owed undo commands run in reverse order after the
application's contained process tree has settled. Successful undo is removed
from the owed stack; failed undo stays pending for an explicit retry. A step
without `undo` is simply removed from the owed stack during cleanup. Raw commands
also use the step's `elevated` flag for both `do` and `undo`.

Use an absolute native executable path appropriate to the host. `args` and
`environment` default to empty; `cwd` defaults to the main executable's
directory.
The environment is cleared before applying the supplied map; host secrets and
PATH are not inherited. Supply any platform/application-required variables
explicitly. Stdin is null; stdout and stderr are null unless shared output is
configured. Main executables and
preparation commands use structured argv with no command-string splitting or
implicit shell. There is no executable allowlist. The launch-level
`elevated` flag defaults to false and is passed to the primary application and
detached children; Unix spawning ignores it.
`.bat` and `.cmd` executable extensions are rejected case-insensitively on every
platform because Windows may otherwise invoke a shell implicitly. A trusted
caller can explicitly choose an interpreter and pass arguments to it. Arguments
follow `std::process::Command` platform conventions, including Windows argument
encoding; this is not a universal argument parser for unusual Windows programs.

Launch and prepare-only requests accept `output`, an absolute path of at most
1,024 UTF-8 bytes without control characters. Omission, JSON null, an empty
string, and the exact string `"null"` retain null output and normalize to the
same retry identity. Configured output opens a regular file once in append mode before any
preparation, detached, or primary process starts. Its owned handle supplies both
stdout and stderr for structured/raw preparation, the primary process, default
detached streams, and reverse undo. Existing data is preserved. File duplication
uses the original opened file, so rotation, path replacement, and failed undo
retry cannot redirect later output to a different file. Explicit typed detached
stdout/stderr paths retain their per-stream precedence. Detached children may
retain their inherited handle after the application's own output owner closes.

The output owner remains held while cleanup is pending or failed, and closes
after all owed undo settles successfully. Prepare-only output leases may contain
no commands; they open before the external provider starts and close after it
stops. They capture their preparation and undo, while external Playnite game
output is not attached through the current provider ABI. File-open failure
creates a retained failed launch or preparation lease with no child effects;
prepared reports identify `output_open_failed`. Unlike the reference's silent
null fallback when opening fails, this service reports the error. Retrying the
same operation never reopens its file. No output transcript or command/environment
text is exposed in process reports, and no rotation or size quota is imposed.
The trusted administrator selects and manages the destination file. Directories,
devices, sockets, and FIFOs are rejected; Unix opens are nonblocking so replacing
a pathname with a FIFO cannot stall the module before command deadlines begin.
This deliberately narrows the reference's unrestricted append-open destination.

Configured shared output fails preflight with `application_output_unsupported`
on a Windows SYSTEM host because its native user-session creation path cannot
safely inherit these handles. Failure to obtain the user token reports
`application_output_unavailable`. No command starts in either case. Ordinary
Windows and Unix use native inherited file handles; live Windows and macOS
qualification remains pending on the current Linux host.

Launch/status/terminate return `{"ok":true,"process":{...}}`. Records contain
`process_id` (monotonic u64 starting at 1), `operation_id`, `pid`, and `state`.
The states are `preparing`, `running`, `stopping`, `cleanup_failed`, `exited`,
`launch_failed`, and `cancelled`. A prepared launch starts with a null PID and
does not spawn the application until all runs succeed. Termination is
nonblocking: it returns `stopping`, and ticks/status calls advance process-tree
settlement and undo. Natural exit similarly passes through `stopping` when
cleanup is owed. `exited` adds `exit_code` (nullable, e.g. signal termination)
and `success`; `launch_failed` adds nullable `os_error`. Explicit termination
finishes as `cancelled` after cleanup.

Prepared records also contain `preparation` and `cleanup` reports.
`preparation.completed` counts successful runs and optional `failure` identifies
the phase, zero-based step, kind, and available exit/OS code. `cleanup.state` is
`deferred`, `pending`, `failed`, or `complete`; `remaining` counts owed steps and
optional `failures` describes the current undo/containment failures.
Each report's optional `last_command` identifies its most recently settled
command by zero-based `step`, `success`, nullable `exit_code`, and an optional
failure `kind` and `os_error`. A spawn failure without a child also settles an
outcome. A wait/containment failure with an owned child does not update this
field until that child settles. It contains no command text, environment, or
output transcript; cleanup retries replace the last outcome after settlement.
`cleanup_failed` includes the retained intended `outcome` (`exited`,
`launch_failed`, or `cancelled`) and a `cleanup_error` code. Process-tree errors
identify the failed spawn settlement, wait, group inspection, signal, or reap
operation; prepared cleanup uses `application_cleanup_failed` while its
`cleanup.failures` report contains the command-level details. A retry clears the
diagnostic while cleanup is in progress and a successful terminal transition
never retains the old error. `cleanup_failed` is not terminal: ownership and
pending undo remain retained, and another terminate request explicitly retries
cleanup.

`ok:true` means the operation was accepted; check `state` to determine progress
or outcome. The original PID is retained after exit and must not be used for
later OS operations; the manager uses owned child handles. Lists return
`{"ok":true,"processes":[...],"next_offset":null}` or an offset for the next
page, ordered by process ID and scoped to the caller. Requests cannot inspect or
terminate another caller's processes.
Pages may contain fewer records than the requested count to keep the complete
serialized response within the broker's 16,384-byte limit. `next_offset` advances
only over records returned, so large lifecycle reports remain readable on the
following page.

An `operation_id` is unique per caller during the module lifetime. Retrying the
same launch specification returns the original process's current state, including
a retained failure or exit, without spawning again. Changing its executable,
argument array, environment map, cwd, detached commands, or preparation steps
yields `operation_conflict`. Defaulted empty args/environment/preparation/detached
list, policy defaults, and absent/null cwd are normalized before comparison;
legacy detached strings and typed objects with empty overrides have the same
retry identity; `working_directory` and `cwd` also normalize to the same value.
Changing a detached override, output path, or explicit elevation conflicts with
the original operation. Host
environment references resolved for the original request are retained on retry;
paths are not canonicalized. Retries never
rerun completed preparation or undo, and repeated termination of a terminal
record is inert. Terminal and cleanup-failed records are retained and cannot be
deleted, so capacity exhaustion cannot silently evict idempotency records. Use a
new operation ID for an intentional new launch. There is no durable operation
ledger or recovery after worker/host restart; IDs and ownership reset with the
instance.

Limits, measured in UTF-8 bytes unless otherwise stated:

- Serialized request: 16,384 bytes; caller identifier: 1–256 bytes.
- Records: 128 total and 32 per caller, including failed and exited launches.
- Operation ID: 1–128 ASCII graphic bytes (no spaces or controls).
- Executable and optional cwd: absolute paths, 1–1,024 bytes, no NUL.
- `exit_timeout_seconds`: integer from 0 through 300; `wait_all` and
  `auto_detach` are booleans.
- `detached`: up to 16 command strings or typed objects, each command at most
  2,048 printable bytes, with up to 64 parsed arguments and 8,192 argument
  bytes combined. Each typed environment and its merged launch environment
  must each stay within the launch environment bounds; each typed cwd must be
  an absolute path of 1–1,024 bytes. Each optional output path must also be
  absolute, 1–1,024 bytes, and free of control characters.
- Args: at most 64, each at most 1,024 bytes, 8,192 bytes combined, no NUL.
- Environment: at most 32 keys, keys of 1–128 ASCII alphanumeric/underscore
  bytes, values at most 1,024 bytes without NUL, 4,096 key/value bytes combined.
  Keys must be unique ignoring ASCII case.
- Preparation: at most eight steps. Structured run/undo commands have the same
  path, argument, and environment limits as the application plus a 1–30,000 ms
  timeout. Raw run has a 10-second deadline; raw undo uses the 15–300 second
  snapshotted application exit policy.
- List page: 1–16 records; default offset 0 and limit 16. Record responses omit
  command arguments and environment and remain below the runtime payload bound.

Errors use `{"ok":false,"code":"..."}`. Codes are `invalid_caller`,
`request_too_large`, `invalid_request`, `invalid_launch`, `invalid_limit`,
`detached_output_unsupported`, `detached_output_unavailable`,
`application_output_unsupported`, `application_output_unavailable`,
`operation_conflict`, `process_limit`, `process_not_found`, `process_wait_failed`,
`process_kill_failed`, and `process_wait_timeout`. `response_too_large` protects
a list from an individually oversized record. Missing and foreign process IDs share
`process_not_found`. OS spawning errors create retained `launch_failed` records.

On Unix, each launch creates a new process group before exec. Termination sends
SIGTERM to the group and waits up to `exit_timeout_seconds` for cooperative
cleanup before sending SIGKILL. A zero timeout skips SIGTERM. Natural root exit
with `wait_all: true` remains running while a non-zombie group member exists;
with `wait_all: false`, the remaining group receives the same graceful-then-force
cleanup. Linux and macOS inspect group members while excluding processes whose
exit is confirmed. The root is deliberately left unreaped
(`waitid(WNOWAIT)`) until the final group signal and a complete member inspection
finds no live descendants, so its PID/PGID cannot be recycled into an unrelated
group. Signal acceptance alone does not start undo or release native ownership;
an incomplete or malformed group observation retains the root for retry.
Linux also requires an unrestricted procfs view in the current PID namespace and
the completed scan to include the held leader in its expected group. It uses
`openat2` to keep inspection inside the verified mount and pidfds to confirm
whole-process exit, including when a zombie leader has live sibling threads.
Linux 5.6 or later and a security policy permitting those system calls are
required. A restricted, unavailable, changed, or inconsistent view retains
ownership and reports cleanup failure. The mount configuration must remain under
trusted administrative control and stable during each scan; a transient change
restored between observations can evade detection. Group enumeration is not
atomic against a separate process joining the group concurrently.
Other Unix platforms cannot establish native group retirement using the current
numeric probe and return an unsupported containment error rather than releasing
the root reservation. Natural root exit starts cleanup on the next
poll when the group is done or `wait_all` is disabled. The record remains
`running` during graceful cleanup and reports the root's
exit status afterward. The host must preserve exclusive wait ownership: it must
not reap these children elsewhere, ignore SIGCHLD, or enable SA_NOCLDWAIT.
A wait ownership error stops group signaling. Processes that deliberately leave
the group (setsid/setpgid), detached adoption, hostile code, and host death are
outside this Unix containment guarantee. Descendant zombies are reaped by their
OS parent/reaper; this module waits only for its own root.

On Windows, launches are created suspended and assigned to a private Job Object
before their primary thread is resumed. The job forbids breakaway and has
KILL_ON_JOB_CLOSE; descendants remain owned even after root exit. Failure to
assign or resume fails the launch closed. An unsettled suspended child remains
owned by its process or preparation record as `cleanup_failed` until a terminate
retry or manager shutdown confirms exit. The standard library still constructs
the command line and environment; Toolhelp locates the suspended primary thread.
`wait_all` uses job accounting, so a root exit with active descendants remains
running until the job becomes empty. Graceful shutdown enumerates top-level
windows in the job and asynchronously sends `WM_CLOSE`; when no window accepts
that request, the job is force-terminated immediately. Otherwise the job is
forcibly terminated when `exit_timeout_seconds` elapses if processes remain.
Console-event delivery and app-specific shutdown remain unsupported. Job handles,
never reopened numeric PIDs, are used for termination. Shutdown
also waits for job accounting to report zero active processes before releasing
ownership; timeout retains the job for retry.

Repeated termination of a terminal record succeeds. Background ticks never sleep
for a running preparation/undo command. They perform at most a bounded spawn,
observation, or containment action and retain child ownership between ticks.
Containment errors and the bounded reap deadline become `cleanup_failed` instead
of discarding the child; an explicit terminate retries. Ordinary application Stop
honors its configured exit timeout, including up to 300 seconds, followed by
bounded confirmation. Direct manager shutdown uses one shared budget for the
application and currently owed undo commands.

For cooperative host shutdown, the host first announces its remaining shared
budget. The application retains the earliest absolute deadline and closes new
launch and preparation admission. Existing status, lookup, termination, and
idempotent replay remain available. Announcement does not start cleanup or
shorten a forward preparation command while consumers still drain. Once cleanup
is authorized, native process, Steam, and active/future undo deadlines are capped
by that retained deadline, reserving bounded force and reap/confirmation time.
Application quiesce advances cleanup without sleeping. An exhausted execution
budget starts no additional undo commands; owed work and uncertain native
identities remain failed or pending rather than becoming cleanup complete.

After announcement or a shutdown attempt, Drop performs only one exact native
containment attempt and grants no new wait budget or undo execution. Standalone
owners that omit explicit shutdown have a five-second fallback. Drop cannot
report failure, and persistent OS errors, uninterruptible kernel operations, or
host death prevent an absolute no-orphan guarantee. Windows closes private jobs
on host death; Unix host death can bypass cleanup. There is no durable watcher
or restart reconciliation for these process identities.

Current source coordinates a 20-second module deadline inside a 25-second worker
window and a 30-second Linux user-service stop timeout. Installed host EOF/runtime
behavior has not been qualified here. Windows SCM now requests cooperative
worker cleanup through a private EOF-only pipe and waits for a clean process
exit before replacement; timeout containment remains a failure. Windows
compilation, native SCM stop/session-switch behavior,
SYSTEM/UAC, and associations remain separate platform qualification gates on the
current Linux host.

Platform references: [POSIX waitid](https://www.man7.org/linux/man-pages/man3/waitid.3p.html),
[Windows job objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects),
and [Toolhelp snapshots](https://learn.microsoft.com/en-us/windows/win32/toolhelp/snapshots-of-the-system).

Vibeshine's `docs/app_examples.md` was used only as a behavioral reference for
application launch and working-directory requirements. Source was not copied.
Detached launchers are a bounded argv-only subset; automatic output rotation
remains unimplemented. Desktop pseudo-apps, URI
launch, and launcher-specific behavior also remain unimplemented.

Tests invoke a controlled Rust test subprocess to verify actual launch, natural
exit reaping, cwd/environment delivery, owner isolation, retries/conflicts,
ordered preparation, reverse undo, continuation after undo failure, failed-run
rollback, cancellation settlement, retained/retryable cleanup failure, tree
shutdown, child/grandchild cleanup after
natural root exit, shared shutdown deadlines, retained launch failures, and
validation/capacity limits.
The ignored `controlled_child` test is their subprocess entrypoint. Platform-specific
APIs are used; Windows/Linux runtime verification must be performed
on those hosts; macOS execution alone does not establish cross-platform parity.

`lookup {operation_id}` returns the caller's existing process record without
launching again. The launch coordinator uses it to reconcile a mutation whose
response was lost to cancellation. Missing and foreign-owned records both return
`process_not_found`. Like status, lookup reaps an observed exited child.
