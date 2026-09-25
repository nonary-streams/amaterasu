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
`detached` list of up to 16 command strings. `wait_all` keeps the app active while
any process in its contained group remains after the root exits. `auto_detach`
keeps a successful root that exits within five seconds as a status-driven app,
after `wait_all` is satisfied. A zero exit timeout skips graceful shutdown.
Detached command strings use platform argv quoting, run best-effort before the
primary executable, and are not attached to its process group or termination
lifecycle. They inherit the app's explicit environment and working directory.
Before argv parsing, `$(NAME)` expands from the app environment and then the
host process environment; `$$` produces a literal dollar sign, and absent names
produce an empty string. Other dollar text remains literal, and an unclosed
`$(...)` reference is invalid. Expansion is bounded and is not shell evaluation:
the expanded string still goes through the host platform's argv parser and
existing count, length, and argument limits. Detached commands have no
per-command environment overrides, output redirection, or independent elevation
field; they receive the same launch-level `elevated` flag as the primary
application. A detached-command spawn failure does not fail the main launch.

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
absolute `cwd`, and null standard handles, just like the application. They have
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
Raw preparation retains the existing 10-second default and 30-second maximum
command timeout and null standard handles. Windows URL and associated-file
launches are not resolved through shell associations. These bounded execution
rules differ from the reference helper, which waits for raw `do` commands
without a timeout, sends their output to its configured pipe, and continues
through undo failures. Preparation output is discarded here. Cleanup attempts
each owed undo once per pass in reverse order and continues after spawn errors,
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
`environment` default to empty; `cwd` defaults to the host working directory.
The environment is cleared before applying the supplied map; host secrets and
PATH are not inherited. Supply any platform/application-required variables
explicitly. Stdin, stdout, and stderr are null handles. Main executables and
preparation commands use structured argv with no command-string splitting or
implicit shell. There is no output capture or executable allowlist. The launch-level
`elevated` flag defaults to false and is passed to the primary application and
detached children; Unix spawning ignores it.
`.bat` and `.cmd` executable extensions are rejected case-insensitively on every
platform because Windows may otherwise invoke a shell implicitly. A trusted
caller can explicitly choose an interpreter and pass arguments to it. Arguments
follow `std::process::Command` platform conventions, including Windows argument
encoding; this is not a universal argument parser for unusual Windows programs.

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
`cleanup_failed` includes the retained intended `outcome` (`exited`,
`launch_failed`, or `cancelled`). It is not terminal: ownership and pending undo
remain retained, and another terminate request explicitly retries cleanup.

`ok:true` means the operation was accepted; check `state` to determine progress
or outcome. The original PID is retained after exit and must not be used for
later OS operations; the manager uses owned child handles. Lists return
`{"ok":true,"processes":[...],"next_offset":null}` or an offset for the next
page, ordered by process ID and scoped to the caller. Requests cannot inspect or
terminate another caller's processes.

An `operation_id` is unique per caller during the module lifetime. Retrying the
same launch specification returns the original process's current state, including
a retained failure or exit, without spawning again. Changing its executable,
argument array, environment map, cwd, detached commands, or preparation steps
yields `operation_conflict`. Defaulted empty args/environment/preparation/detached
list, policy defaults, and absent/null cwd are normalized before comparison;
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
- `detached`: up to 16 command strings, each at most 2,048 printable bytes, with
  up to 64 parsed arguments and 8,192 argument bytes combined.
- Args: at most 64, each at most 1,024 bytes, 8,192 bytes combined, no NUL.
- Environment: at most 32 keys, keys of 1–128 ASCII alphanumeric/underscore
  bytes, values at most 1,024 bytes without NUL, 4,096 key/value bytes combined.
  Keys must be unique ignoring ASCII case.
- Preparation: at most eight steps. Each run/undo command has the same path,
  argument, and environment limits as the application plus a 1–30,000 ms timeout.
- List page: 1–16 records; default offset 0 and limit 16. Record responses omit
  command arguments and environment and remain below the runtime payload bound.

Errors use `{"ok":false,"code":"..."}`. Codes are `invalid_caller`,
`request_too_large`, `invalid_request`, `invalid_launch`, `invalid_limit`,
`operation_conflict`, `process_limit`, `process_not_found`, `process_wait_failed`,
`process_kill_failed`, and `process_wait_timeout`. Missing and foreign process IDs share
`process_not_found`. OS spawning errors create retained `launch_failed` records.

On Unix, each launch creates a new process group before exec. Termination sends
SIGTERM to the group and waits up to `exit_timeout_seconds` for cooperative
cleanup before sending SIGKILL. A zero timeout skips SIGTERM. Natural root exit
with `wait_all: true` remains running while a non-zombie group member exists;
with `wait_all: false`, the remaining group receives the same graceful-then-force
cleanup. Linux and macOS inspect group members while excluding the deliberately
unreaped root and other zombies. The root is deliberately left unreaped
(`waitid(WNOWAIT)`) until the final group signal, so its PID/PGID cannot be
recycled into an unrelated group. Natural root exit starts cleanup on the next
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
assign or resume fails the launch closed. The standard library still constructs
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
of discarding the child; an explicit terminate retries. Manager shutdown requests
all stops and advances owed undo against one shared deadline based on the longest
configured application exit timeout plus a 2-second reap deadline. If a long lifecycle command outlasts that deadline, shutdown
returns an error while the live manager still retains ownership for retry. Drop
first attempts normal cleanup, then cancels remaining lifecycle commands and
makes a final bounded containment attempt. Drop cannot report failure, and
persistent OS errors, uninterruptible kernel operations, or host death prevent an
absolute no-orphan guarantee. Windows closes private jobs on host death; Unix
host death can bypass cleanup. There is no durable watcher or restart
reconciliation.

Platform references: [POSIX waitid](https://www.man7.org/linux/man-pages/man3/waitid.3p.html),
[Windows job objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects),
and [Toolhelp snapshots](https://learn.microsoft.com/en-us/windows/win32/toolhelp/snapshots-of-the-system).

Vibeshine's `docs/app_examples.md` was used only as a behavioral reference for
application launch and working-directory requirements. Source was not copied.
Detached launchers are a bounded argv-only subset: output handling and
per-command environment overrides remain parity gaps. Desktop pseudo-apps, URI
launch, and launcher-specific behavior also remain unimplemented.
In particular, this module's omitted cwd inherits the host cwd rather than using
the executable directory.

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
