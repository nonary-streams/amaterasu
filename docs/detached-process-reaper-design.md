# Detached process ownership across graph reloads

Status: design proposal; not implemented or qualified.

On Linux, the application provider owns each detached command and Steam launcher
as a `std::process::Child` and polls it while its `ProcessManager` lives. The
Linux user service can unload that provider and load another graph in the same
host process. If a detached child is still running at unload, dropping `Child`
does not wait for it; its later exit can leave a zombie in that host process.
This is distinct from a managed process tree, whose leader must remain
exclusively wait-owned until group retirement is confirmed. Windows closes a
process handle on drop and does not have this Unix zombie behavior.

Waiting for or killing detached processes at graph shutdown would change their
contract. A provider-owned background thread cannot continue executing after its
library unloads. `waitpid(-1)`, `SIGCHLD` disposition changes, and an unrelated
child reaper can steal the managed-tree leader and invalidate its PID/PGID
ownership checks. A helper created after a child is spawned is not its parent
and cannot perform the child's ordinary wait.

## Proposed ownership boundary

Introduce a versioned process service and SDK adapter whose selected provider
owns detached children from the moment they are spawned, through actual exit and
wait. Its implementation is an independently built library held by a trusted
process-lifetime composition outside the reloadable module graph. The host
selects and invokes it through the same versioned contract and SDK used by
external implementations; consumers must not silently fall back to local
spawning when it is unavailable. Native modules are trusted code, and grants
are authorization checks rather than a native-code sandbox.

The contract should expose bounded admission and opaque, provider-issued child
and output-sink tokens, scoped to the authorized caller and service instance.
It must not expose PIDs, pidfds, file descriptors, `Child` representations, or
allocator ownership as consumer contract values. Define explicit operations to:

1. Reserve bounded capacity before spawning. A failed reservation creates no
   child, and abandoned reservations are reclaimable without killing a child.
2. Create a provider-owned output sink when redirection is requested. A sink
   preserves the already-opened file identity and shared append semantics across
   preparation, main, and detached commands; reopening a path later is not an
   equivalent guarantee.
3. Spawn a detached child using a validated, bounded command, environment,
   working directory, elevation policy, and optional sink token. The provider
   owns the native child before acknowledging admission and supplies an opaque
   token and idempotent operation identity. Failed or lost replies must not
   produce a second process on retry.
4. Poll or consume a terminal receipt while the originating graph is alive,
   including the launcher exit result currently used for Steam attribution.
   Releasing graph-facing observation must not release the provider's wait
   ownership. A finished child is reaped exactly once, even if its consumer is
   gone; failed native waits remain visible and retain capacity/evidence.
5. Quiesce the consumer by closing its observation/admission bindings without
   waiting for live detached children or terminating them. The provider and its
   own worker retire only after all owned children are reaped or the entire
   host process exits.

The output-sink requirement may require moving main and preparation process
spawning to the same service, or another versioned provider-owned sink API;
the existing module-local `SharedOutput` cannot be transferred as an opaque
cross-provider object merely by passing its file descriptor. Preserve existing
elevation, executable resolution, environment, cwd, output error, and Steam
launcher status behavior during that migration. Managed process-tree cleanup
must continue to retain its own exact root identity and must never be reaped by
the detached-child path.

## Composition and upgrade requirements

The service's signed artifact and native code must remain pinned while it owns
children, including across same-process graph reloads. A package update that
would replace an active provider must either keep the old provider and its
bindings alive until its owned children retire, or use an explicitly versioned,
verified migration that preserves every ownership receipt. It must not unload
old code, discard wait ownership, or substitute a different provider silently.
Bound provider instances, reservations, open sinks, pending operations, and
terminal receipts; return backpressure before spawning rather than forgetting
an existing child. Define startup, shutdown, crash, and failed-handoff behavior
in the contract and keep package activation and existing signature, grant,
service-pin, and dependency checks intact.

On hosts that cannot supply this process-lifetime service, retain the current
behavior only as an explicitly reported compatibility limitation until the
consumer and provider can migrate together. Do not claim the zombie issue is
fixed by documenting the API or by a graph-local reaper.

## Required qualification

- Exercise an unchanged SDK consumer against two independently implemented
  providers, including admission, cancellation, idempotent retry, bounded
  capacity, output-sink identity, and retirement.
- In one real Linux host process, start a long-lived detached child, quiesce and
  unload the application graph, reload a graph, let the child exit, and verify
  that the exact child was reaped and did not remain a zombie. Its process must
  remain running throughout graph shutdown.
- Run a managed process group concurrently and prove that its leader remains
  exclusively wait-owned, its PGID cannot be reused before retirement, and the
  detached reaper never consumes that leader's status.
- Inject capacity exhaustion, failed spawn, lost admission and terminal replies,
  failed observation, provider shutdown, and attempted package replacement.
  Verify exact ownership is retained on every failure and that no fallback
  starts an untracked process.
- Verify Steam launcher success and failure receipts, shared output behavior,
  and native Windows handle retirement separately. Source tests do not establish
  installed behavior or live client playback.
