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

Preparation commands use literal argv, a cleared explicit environment, optional
absolute `cwd`, and null standard handles, just like the application. There is no
shell interpretation. `timeout_ms` defaults to 10,000 and must be 1–30,000.
Steps run one at a time. A successfully spawned `run` owes its paired `undo` even
if it later exits unsuccessfully, times out, or is cancelled, because it may have
changed external state. Owed undo commands run in reverse order after the
application's contained process tree has settled. Successful undo is removed
from the owed stack; failed undo stays pending for an explicit retry. A step
without `undo` is simply removed from the owed stack during cleanup.

Use an absolute native executable path appropriate to the host. `args` and
`environment` default to empty; `cwd` defaults to the host working directory.
The environment is cleared before applying the supplied map; host secrets and
PATH are not inherited. Supply any platform/application-required variables
explicitly. Stdin, stdout, and stderr are null handles. There is no command-string
splitting, implicit shell, output capture, elevation, or executable allowlist.
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
optional `failures` describes the current undo/containment failure.
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
argument array, environment map, cwd, or preparation steps yields
`operation_conflict`. Defaulted empty args/environment/preparation and absent/null
cwd are normalized before comparison; paths are not canonicalized. Retries never
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
SIGTERM to the group, allows 200 ms for cooperative cleanup, then sends SIGKILL.
The root is deliberately left unreaped (`waitid(WNOWAIT)`) until the final group
signal, so its PID/PGID cannot be recycled into an unrelated group. Natural root
exit starts the same cleanup on the next poll; launcher handoff is not supported.
The record remains `running` during this brief cleanup and reports the root's
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
Windows has no universal graceful-stop primitive for arbitrary native GUI and
console programs, so after the 200 ms interval the job is forcibly terminated;
application-specific cooperative stop and console-event delivery remain parity
work. Job handles, never reopened numeric PIDs, are used for termination. Shutdown
also waits for job accounting to report zero active processes before releasing
ownership; timeout retains the job for retry.

Repeated termination of a terminal record succeeds. Background ticks never sleep
for a running preparation/undo command. They perform at most a bounded spawn,
observation, or containment action and retain child ownership between ticks.
Containment errors and the bounded reap deadline become `cleanup_failed` instead
of discarding the child; an explicit terminate retries. Manager shutdown requests
all stops and advances owed undo against one shared 200 ms grace plus a 2-second
reap deadline. If a long lifecycle command outlasts that deadline, shutdown
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
Its detached launchers, desktop pseudo-apps, URI launch, output logging, and
launcher-specific behavior are unimplemented parity work.
In particular, this module's omitted cwd inherits the host cwd rather than using
the executable directory.

Tests invoke a controlled Rust test subprocess to verify actual launch, natural
exit reaping, cwd/environment delivery, owner isolation, retries/conflicts,
ordered preparation, reverse undo, failed-run rollback, cancellation settlement,
retained/retryable cleanup failure, tree shutdown, child/grandchild cleanup after
natural root exit, shared shutdown deadlines, retained launch failures, and
validation/capacity limits.
The ignored `controlled_child` test is their subprocess entrypoint. Platform-specific
APIs are used; Windows/Linux runtime verification must be performed
on those hosts; macOS execution alone does not establish cross-platform parity.

`lookup {operation_id}` returns the caller's existing process record without
launching again. The launch coordinator uses it to reconcile a mutation whose
response was lost to cancellation. Missing and foreign-owned records both return
`process_not_found`. Like status, lookup reaps an observed exited child.
