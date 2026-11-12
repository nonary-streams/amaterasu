# Default network provider

Consumers use `amaterasu.network-interface/1` through the public Network SDK.
The selected provider owns sockets and name-service implementation; no consumer
silently constructs this default provider when another implementation fails.

System name-service lookup runs in a separate executable embedded in the signed
network library. The helper uses the operating system's `ToSocketAddrs` behavior,
including configured NSS on Unix. The provider never starts a DNS thread inside
its native library. Numeric address lookup does not start a helper.

Each lookup owns one exact helper process and nonblocking result pipe. There are
at most eight admitted helper processes, including timed-out children awaiting
retirement. Termination acceptance does not release capacity: exit and reaping
must be observed. Results are bounded to 256 distinct address values and a
6,923-byte versioned response. A valid response from a still-running helper is
not successful completion. Malformed, truncated, oversized, and trailing output
is rejected and its child is retained for cancellation and retirement.

The managed network module obtains its state location through the explicit
`amaterasu.host-state-context/1` call grant. It does not accept a package setting
for that path. Existing policies need this grant before selecting the new module;
an absent or invalid host context fails startup before native registration.
`Factory::with_runtime_state` is the production composition entry point.
Standalone `Factory::new` and `Factory::with_limits` remain available; platforms
requiring a filesystem image use an independently created private temporary
directory in standalone mode.

The host announces one earliest shutdown deadline before consumer cleanup begins.
Announcement caps DNS calls without prematurely closing provider admission.
Actual provider quiescence closes new bindings, handles, and DNS work while
preserving existing close/shutdown operations. Quiescence cancels helper work and
polls exact retirement without blocking a host tick. Native kernel operations can
still fail or delay containment; query timeout and a termination request must not
be reported as complete native cleanup.

Standalone owners can call `Factory::try_shutdown` to perform one shutdown step.
Pending retirement and provider errors return a `ShutdownFailure` containing the
same boxed factory and the reason. Retain that owner and poll again until the
operation succeeds; an unsuccessful step does not transfer retirement to a
background thread. Runtime registration and consumer bindings must end before
the factory is destroyed.

On Linux the helper executes from a sealed executable memfd. There is no
filesystem executable fallback when that operation is prohibited. The child
arms a parent-death signal and checks the exact parent, while the parent retains
its unreaped process-group identity through final containment and reaping.

On Windows the helper is materialized beneath the host-selected private state
location. Its exact bytes and no-reparse ancestor chain remain read-locked
against write/delete through process creation and child lifetime. A protected
current-user/SYSTEM directory is created for standalone mode. A strict inherited
handle list and Job Object are assigned atomically at process creation. Cached
digest-named images are bounded; active or unverified entries are preserved and
capacity failure is explicit.

On macOS the filesystem image belongs to a private current-user directory. The
image and directory descriptors remain held, and bytes, file identity, ownership,
and modes are checked. This is a trusted native-code boundary; directory
permissions and advisory locks do not sandbox other code running as the same
user. The independent helper watches its parent before entering name service.

`build.rs` compiles the helper using Cargo's `RUSTC`, `TARGET`, encoded target
Rust flags, and `RUSTC_LINKER` when supplied. Cross builds require the actual
target standard library and linker; compilation failure never substitutes a host
executable. Helper bytes are included in the component's existing signed digest,
without a new unsigned executable lookup or ambient PATH dependency. A second
lookup-free fixture is compiled for tests and is not embedded in production code.

Tests cover bounded framing, held helper cancellation, exit/reap before success,
deadlines, admission, exact executable ownership, managed startup authorization,
and an unchanged Network SDK consumer against a replacement resolve callback.
Socket tests use loopback addresses and ephemeral ports. Native Windows and
macOS process execution, installed service shutdown, and physical router behavior
require separate qualification; Linux source and fixture tests do not prove them.
