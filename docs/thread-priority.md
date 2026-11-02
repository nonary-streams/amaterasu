# Streaming thread priorities

Amaterasu follows Vibeshine's CPU scheduling levels for the corresponding
streaming work. This does not claim identical thread topology or lower measured
latency. Providers retain ownership of their threads; the generic host does not
raise the priority of every module or child process.

| Work | Linux nice | Windows thread priority |
| --- | --- | --- |
| Dedicated capture, including VKMS presentation and WGC capture | -15 | Highest |
| Audio capture | -15 | Highest |
| Video and audio encoding | -10 | Above normal |
| Synchronous video pipeline and media delivery | -10 | Above normal |
| Control and input processing | -15 | Highest |

`amaterasu_module_sdk::threading` exposes the priorities, a dedicated-worker
setter, and `ScopedThreadPriority`. A scoped guard restores the original priority
on return or unwind and cannot move between threads. Multiplexed module ticks
and native transport/codec entries use guards, so later application launches and
maintenance modules retain their original policy. Native codec driver threads
may inherit the dedicated encoder worker's priority. The legacy per-stream VKMS
frame helper inherits capture priority only during its own launch.

`ThreadScheduler` permits explicit alternative adapters with adapter-owned saved
state. A replacement test runs the same consumer against implementations with
different saved-state representations, including a failing nested operation.

On Linux these are ordinary nice levels, not real-time FIFO/RR scheduling. The
setter first uses the existing nice allowance. If that is insufficient, it can
make an **already permitted** `CAP_SYS_NICE` effective on the current thread for
one scheduling call. It restores the exact capability sets before returning,
including on failure or unwind. A failed restoration terminates the process
rather than continuing with unintended scheduling or effective capabilities.
No capability is added to the permitted or inheritable sets by module code.

Installation must supply a sufficient `RLIMIT_NICE` or permit `CAP_SYS_NICE` on
the trusted host executable. A limit of 35 permits nice -15; a limit of 31 permits
only -11. For an operator-approved installation, the narrow executable permission
is `sudo setcap cap_sys_nice=p /absolute/path/to/amaterasu`. It does not use the
`+ep` form or run the host as root. Replacing the host executable can remove file
capabilities; the installer must preserve or reapply the selected policy. Native
modules remain trusted code and can exercise permissions available to the host.

Permission failures are reported without claiming the priority was applied.
Module-thread warnings are bounded per thread and level. The normal module
operation continues if raising priority is unavailable.

The separately built VKMS provider retains a small Linux scheduling bridge
because its pinned SDK revision predates this helper. It uses the same priority
and capability-lifetime rules, with independent focused tests, and remains
buildable without an unpublished SDK Git revision. Replace this bridge when a
published SDK pin supplies the helper.

The Linux `thread-priority-smoke` SDK example verifies real worker priorities,
nested module restoration, helper inheritance, and unchanged capability sets.
It is opt-in and requires the normal user to have the scheduling permission.
Default unit tests use injected scheduling and capability adapters. Windows
priority values are implemented through Win32 APIs; Linux qualification does not
establish Windows runtime behavior. GPU context/queue priority is separate and
is not changed by this CPU scheduling support.
