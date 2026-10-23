# Windows virtual input boundary

## Current code and reusable behavior

The input module owns a caller-bound lease, validates bounded event batches, tracks
held keys/buttons and pen/touch contacts, and retries neutralization when release
fails. `Controller<B>` passes accepted events through the platform `Backend`
interface. The host launches component sets in an interactive user session, while
the Windows service stays in Session 0. Keep these contracts when replacing the
Windows transport. Linux's backend uses uinput and does not need Windows HID
mechanics.

There is **no VHF implementation in Amaterasu**. `WindowsBackend` currently uses
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

The Windows target should reuse the proven UMDF2 VHF pattern from the Nonary
gamepad driver where suitable, in a **separate minimal keyboard/mouse source**
with a more restricted interface. Microsoft's VHF architecture guide describes
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
backpressure before queuing to the driver; preserve release/neutral reports under
pressure. On expiry or revocation, the companion sends neutral reports for every
held keyboard, mouse, controller, pen and touch state and waits for completion.
The driver also maintains a watchdog and neutralizes its own device state if its
companion handle disappears, then stops submitting and calls `VhfDelete` with
correct callback and buffer lifetime handling during teardown. Recovery cannot
depend solely on the replaceable input module.

The first source addition needs reviewed Generic Desktop keyboard and mouse
collections. The current `WindowsScan` and Moonlight key forms need an explicit
HID usage mapping; scan codes cannot be copied into keyboard reports. Mouse
needs fixed reports for buttons, relative motion, and vertical/horizontal
scrolling. Absolute pointing, pen/touch and controller profiles require their
own descriptor and consumer qualification before being advertised. The private
control interface needs fixed-size, versioned query/create, keyboard-state,
pointer-state, neutralize, and destroy operations, with per-handle ownership
and exact length/report-ID checks. Reserve implementation-specific IOCTL values
only when that driver interface is reviewed. Do not add arbitrary descriptor
or raw-report IOCTLs.

The host-owned `amaterasu-input-companion` crate now provides a typed,
testable authorization state machine for keyboard/mouse events. It binds a
synthetic transport peer, set identity, session, lease token and generation;
permits one active device lease at a time; checks sequence, grants and event
values; rate-limits nonrelease events; and neutralizes held state on revoke,
expiry or uncertain delivery. The replaceable input module has a narrow
translator for currently representable events. Moonlight key mapping,
absolute pointer, high-resolution wheel, pen/touch and controller events fail
closed in that translator. The crate is **not connected to authenticated
Windows IPC or a VHF device** and deliberately has no production grant-registry
constructor. Its exclusivity guarantee is scoped to one registry instance;
the installed companion must own one exclusive device handle and registry for
that handle. A production companion must derive peer identity
from the accepted OS connection and grants from protected host policy, persist
generation floors, call expiry/close cleanup, and own a driver handle whose
access is restricted by the installed driver. No production elevated operation
is enabled by this code; the user worker still uses the current backend.

The cross-platform module contract remains one logical package with target
adapters: VHF on Windows, uinput or a narrowly authorized device-access helper
on Linux, and a separately designed macOS adapter later. Platform-specific
privileged components need their own approval and installation path. The user
selection and API contract should not require end users to compile with Cargo.

## Validation before enabling

- Build and sign a driver with the WDK and test its INF, VHF lower-filter
  installation, PnP teardown, descriptor behavior and report parsing in a VM.
  This checkout has the Windows Kits 10.0.26100.0 `shared/vhf.h`, but the
  expected `um/x64/VhfUm.lib` was not found in the installed Kits library
  tree during this slice. No new driver build was attempted.
- Test invalid report IDs/sizes/types, IOCTL rights, session spoofing, replay,
  flood/backpressure, companion crash, logout, policy revocation and stuck-key
  release. Verify no user module can open the device directly.
- Test real keyboard, mouse, pen/touch and controller consumers, including
  multi-session behavior and any XInput compatibility requirement.
- Integrate the companion only after the elevated broker has authenticated
  identity-bound grants and explicit privileged-handler approval. Do not install
  a driver or grant OS privileges as part of ordinary repository activation.

Microsoft references: [VHF architecture and lifecycle](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/virtual-hid-framework--vhf-),
[VhfReadReportSubmit and buffering](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/nf-vhf-vhfreadreportsubmit),
[VHF configuration](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/vhf/ns-vhf-_vhf_config),
and [driver security checklist](https://learn.microsoft.com/en-us/windows-hardware/drivers/driversecurity/driver-security-checklist).
