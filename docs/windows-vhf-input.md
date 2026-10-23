# Windows virtual input boundary

## Current code and reusable behavior

The input module owns a caller-bound lease, validates bounded event batches, tracks
held keys/buttons and pen/touch contacts, and retries neutralization when release
fails. `Controller<B>` passes accepted events through the platform `Backend`
interface. The host launches component sets in an interactive user session, while
the Windows service stays in Session 0. Keep these contracts when replacing the
Windows transport. Linux's backend uses uinput and does not need Windows HID
mechanics.

The new VHF device is not installed or selected by default. `WindowsBackend` currently uses
`SendInput` for keyboard and mouse, ViGEm for controller input, and the
Synthetic Pointer API for pen/touch. Those paths are current behavior, not the
chosen new Windows transport.

The authorized nearby checkout at `C:\Users\Chase\sources\libvirtualgamepad`
is `Nonary/libvirtualgamepad` revision `c110d04fba3e60a1e015be6367813db541f7519d`.
It implements a signed-package-capable UMDF2 VHF **gamepad** source and a
versioned, fixed-size `METHOD_BUFFERED` control protocol. The driver binds a
controller slot to a file handle and destroys owned slots on close. Its
published profile descriptors and IOCTLs do not expose keyboard or mouse.
The current INF grants read/write control access to interactive users, so it
cannot be treated as a companion-only privileged interface. Its build,
packaging, per-handle ownership and VHF report-pump patterns are useful
references; copying its gamepad ABI cannot deliver keyboard or mouse input.

## Boundary to implement

One signed logical input package may contain an ordinary user-worker component
and an optional, separately approved privileged companion. The user component
continues to own input intent and the existing lease. The companion translates
validated events into a fixed versioned set of HID reports and is the sole client
of a restricted driver interface. Neither a catalog signature nor a module's own
request authorizes the privileged companion or its driver access. Ordinary native
modules must never be loaded into the service or driver.

The new `drivers/windows-vhf-keyboard-mouse` source reuses the proven UMDF2 VHF
build and lifecycle pattern from the Nonary gamepad driver in a **separate
keyboard/mouse device** with its own restricted interface. Microsoft's VHF architecture guide describes
the kernel-mode path, while its `vhf.h` references include user-mode fields;
the locally built UMDF2 source demonstrates the latter for its supported
Windows/WDK target. Qualify the chosen driver project against the intended
Windows versions before shipping. It publishes fixed, reviewed HID report
descriptors and submits validated input reports with `VhfReadReportSubmit`.
Keep policy, networking, repository parsing, module loading, and feature logic
out of the driver. VHF is a driver interface, not an application-level
replacement call for `SendInput`.

Use two independently enforced gates:

1. The user-worker to companion channel authenticates the connected OS principal,
   interactive session, selected component-set identity, and granted input
   capability. Derive identity from the transport and host state, never a caller
   ID in a request body. Bind a short-lived lease to one session and device set;
   revoke it on disconnect, policy change, component replacement, logout, or
   service shutdown. The companion must reject replayed sequence numbers and
   stale generations. Do not expose an arbitrary elevated command endpoint.
2. The driver device/interface security descriptor admits only the installed
   companion identity with the minimum required rights. Every IOCTL has an
   explicit access mask and the driver checks it; parse and bound every input
   buffer, report ID, report size and type against the fixed descriptor. Do not
   accept a new descriptor, device class, or raw kernel operation from a normal
   repository package. A privileged companion or descriptor update is a separate
   installed-component approval and versioned deployment.

The driver owns a bounded report queue or explicitly follows VHF's
`EvtVhfReadyForNextReadReport` one-report-at-a-time rule. Apply rate limits and
backpressure before queuing to the driver; preserve release reports under
pressure. For the new keyboard/mouse path, the companion destroys its VHF child
synchronously on revocation, after attempting held-state releases. This avoids
claiming that queued neutral reports have reached the HID consumer. The driver
also destroys the child after a missed heartbeat, failed asynchronous report
submission, a two-second stalled report queue, file close or device stop. Recovery cannot
depend solely on the replaceable input module.

The source publishes fixed Generic Desktop six-key keyboard and relative mouse
collections, with five buttons and vertical/horizontal scrolling. Its private
interface has versioned fixed-size query/create, keyboard-state, pointer-state,
destroy and heartbeat operations; write IOCTLs require write access.
The INF grants the deterministic `AmaterasuInputCompanion` service SID device
read/write access, plus SYSTEM/Administrators and the UMDF host identity. It
does not grant interactive users access. One file object owns the VHF child;
close, device stop, or explicit destroy synchronously deletes it. A two-second
driver watchdog destroys the child and refuses further input when the
companion stops sending updates or heartbeats, or VHF stops requesting queued
reports even while heartbeats continue. Its bounded report queue
rejects additional input instead of dropping a release. This source requires
VM/device tests before claiming HID consumer behavior or ACL enforcement.

The companion's `VhfDeviceSink` maps a qualified subset of set-1 Windows scan
codes to HID usages and sends full keyboard/mouse state through a real Windows
SetupDi/CreateFile/DeviceIoControl transport. Unsupported scan codes and a
seventh simultaneous non-modifier key fail closed. Moonlight key mapping,
absolute pointer, high-resolution wheel, pen/touch and controller events remain
unsupported on this path. Scan codes are never copied directly into reports.

The `amaterasu-input-companion` crate has a typed permit and input session state
machine and a separately built Windows SCM service. The host reads an optional
machine-owned `input-companion-policy.json` with schema 1, selected component-set
SHA-256 digest and keyboard/mouse grants. When present, it sends a short-lived
permit for the supervised interactive worker's OS PID and creation time, read
from its retained process handle, to a SYSTEM/Administrator
only host pipe, and refreshes it while that worker and policy remain current.
The companion independently checks that the caller is the live `Amaterasu` SCM
service, inspects the worker's SID, session, PID and creation time, and allows
only that process to open the interactive worker pipe. It rechecks the sender's
impersonated token on each event. The worker gets one device lease with a random
token, strictly increasing sequence and generation, a two-second host-permit
deadline and a bounded event rate. A current host permit refreshes the active
session's expiry for long-running input; losing the permit closes it. The
generation floor is written before issue
to a protected machine directory and survives companion restart; live grants do
not. A policy change stops the supervised worker; a stale permit, disconnect or
shutdown destroys the VHF child before another lease may activate. Two pipe
names separate host permits from user event traffic. Worker client rights omit
`FILE_CREATE_PIPE_INSTANCE`, frames and connection counts are bounded, and
unpermitted opening PIDs are rejected before reading a frame.

The input module can explicitly select `windows_backend: "vhf_companion"` in its
settings. That mode sends keyboard/mouse events through the companion and fails
closed for unsupported event types. It never falls back to `SendInput`; omitted
settings keep the existing backend. The service does not install itself or the
driver, nor does it create or modify the machine-owned policy. An approved
repository is not an elevated-input grant. The companion's current LocalSystem
account and device service SID require separate installation review.
The service's `ROOT` argument must be an installer-owned, machine-private
runtime root under the trusted installation. Its child generation directory is
protected by the companion; an installer must establish and verify the parent
before starting either service.
Windows HID typematic owns repeat for a held key in this mode; scheduled repeat
events do not submit duplicate down reports. A dead pipe is discarded so
controller cleanup can clear local held state and a later lease can reconnect.

Native modules in one user process share that process's privileges and memory.
A duplicated pipe handle written by another process under the same user and
session can retain the opening PID as seen by Windows; this protocol does not
claim to sandbox hostile code from the same user. Its privileged boundary is
the independently installed companion and fixed driver protocol. Runtime
pipe/SCM, ACL, driver and real HID behavior have not been VM tested.

The cross-platform module contract remains one logical package with target
adapters: VHF on Windows, uinput or a narrowly authorized device-access helper
on Linux, and a separately designed macOS adapter later. Platform-specific
privileged components need their own approval and installation path. The user
selection and API contract should not require end users to compile with Cargo.

## Validation before enabling

- The x64 UMDF DLL builds with VS2022 BuildTools, Windows SDK 10.0.26100.0 and
  Microsoft's cached WDK NuGet package `Microsoft.Windows.WDK.x64`
  10.0.26100.6584, which contains `VhfUm.lib` and UMDF 2.15 headers. The base
  Kits tree alone lacks those WDK pieces. ARM64 was not built. The INF targets
  Windows 10 build 19041+ and Windows 11 build 22000+ separately; the nearby
  gamepad INF says 1909 in a comment but its actual Windows 10 decoration is
  19041. The local build used `/p:LocalBuildWithoutPdb=true` because installed
  VS PDB components have mismatched versions; a release build should generate
  symbols with a consistent VS toolchain. Compile success is not driver
  installation or platform validation.
- Sign and package the driver and test its INF, VHF lower-filter installation,
  PnP teardown, descriptor behavior and report parsing in a VM.
- Test invalid report IDs/sizes/types, IOCTL rights, session spoofing, replay,
  flood/backpressure, companion crash, logout, policy revocation and stuck-key
  release. Verify no user module can open the device directly.
- Test real keyboard, mouse, pen/touch and controller consumers, including
  multi-session behavior and any XInput compatibility requirement.
- Review an installer that registers `AmaterasuInputCompanion` as a dedicated
  service with its service SID, protects the machine policy/root, installs the
  signed driver and limits device access. Do not install a driver or grant OS
  privileges as part of ordinary repository activation. No installation or
  end-to-end OS test has been performed in this development task.

Microsoft references: [VHF architecture and lifecycle](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/virtual-hid-framework--vhf-),
[VhfReadReportSubmit and buffering](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/nf-vhf-vhfreadreportsubmit),
[VHF configuration](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/ns-vhf-_vhf_config),
and [driver security checklist](https://learn.microsoft.com/en-us/windows-hardware/drivers/driversecurity/driver-security-checklist).
