# Physical display module

`amaterasu-display` builds the native `cdylib` and an `rlib`. It provides
`amaterasu.display/1` and calls `amaterasu.state/1`. Both the module and originating
callers require the state call grant because recovery writes retain parent
request authority. The module has no direct filesystem persistence. Native
modules are trusted code; API grants are not a native-code sandbox.

Startup settings default to read-only:

```json
{"allow_mutation":false}
```

An administrator must explicitly enable `allow_mutation` before apply, restore,
or recover. Windows has a native CCD implementation. Linux (including KDE Wayland)
and macOS return `unsupported_platform` for enumeration/mutation; no simulated
platform data, shell fallback, or `kscreen-doctor` adapter is used.

## Refresh-rate option model

The library also exposes `RefreshRateOption` as a reusable typed configuration
model. Its default is `Auto`, and its serialized tokens are `auto`, `disabled`,
`manual`, and `prefer_highest`. The legacy token parser maps unknown values to
`Disabled`. The variants describe using client FPS, doing no additional refresh-rate
configuration, using the separate `dd_manual_refresh_rate` setting, or preferring the
highest refresh rate for the selected resolution, respectively.

This model does not persist settings, project scoped values into a runtime
request, or perform provider/native display effects.

## Display configuration option model

The library exposes `DisplayConfigurationOption` as a reusable typed
configuration model. Its default is `VerifyOnly`, and its exact serialized
tokens are `disabled`, `verify_only`, `ensure_active`, `ensure_primary`, and
`ensure_only_display`. The legacy token parser maps unknown values to
`Disabled`. The pinned meanings are:

- `disabled` performs no additional display configuration and disables the
  other `dd_` display options.
- `verify_only` requires the selected display to be active without changing
  display state.
- `ensure_active` activates the selected display if it is inactive.
- `ensure_primary` activates the selected display if it is inactive and makes
  it primary.
- `ensure_only_display` activates the selected display if it is inactive and
  disables other displays.

The pinned reference covers Windows and Linux private displays managed through
KScreen. It does not specify a Moonlight gate for this option; the Moonlight
"Optimize game settings" prerequisite belongs to `dd_resolution_option`. The
public enum itself has no persistence, scoped-value projection, runtime or
provider display effects, management UI readback, or platform qualification.
macOS behavior remains unqualified.

## Resolution option model

The library exposes `ResolutionOption` as a reusable typed configuration model.
Its default is `Auto`, and its serialized tokens are `auto`, `disabled`,
and `manual`.
The legacy token parser maps unknown values to `Disabled`. Its documented meanings
are:

- `auto` changes to the client's requested resolution.
- `disabled` performs no additional resolution configuration.
- `manual` uses the separate `dd_manual_resolution` setting.

Moonlight requires `Optimize game settings` to be enabled for this option to work.
This module exposes the model only; it does not persist settings, project scoped
values into runtime requests, perform provider effects, or implement the
Moonlight gate.

## API

```json
{"action":"status"}
{"action":"enumerate","offset":0,"limit":16}
{"action":"enumerate","display":"<id returned by enumeration>","offset":0,"limit":32}
{"action":"apply","configuration":{"displays":[{"id":"<id returned by enumeration>","width":1920,"height":1080,"refresh":{"numerator":60000,"denominator":1001},"x":0,"y":0,"rotation":"identity"}]}}
{"action":"restore","lease":"display-0000000000000001"}
{"action":"recover","lease":"display-0000000000000001"}
```

`status` returns native support, startup mutation policy, `state` (`idle`, `leased`
or `recovery_required`), `owned_by_caller`, and the lease only to its owner. After a
module restart every outstanding journal is reported as requiring recovery.

Enumeration without `display` returns connected monitor identities, friendly
names, active-source/current-mode data and advanced-color status (null when the
OS cannot report it). `id` is the lowercased Windows monitor device-interface
path, not a transient adapter LUID. Driver reinstallation or a connection change
can replace that identity; it is not a universal portable monitor UUID.

Enumeration with `display` returns GDI modes for that **active** source. These
records expose integer `refresh_hz`; CCD apply accepts rational refresh rates and
validates the exact request with Windows. Inactive-target mode enumeration is
explicitly unsupported. Results are `entries` and `next_offset`; limits are 1–32
and pages also cap encoded rows at 12,000 bytes. Offsets are not immutable
snapshot cursors; restart enumeration after hotplug or external display changes.
Native buffer counts and driver mode enumeration have finite admission limits.

Apply specifies the **entire desired active display set**. Omitted displays are
disabled. Each display must use an independent source, rectangles must not
overlap, and one display must begin at `(0,0)`. Rotation is `identity`, `rotate90`,
`rotate180`, or `rotate270`; dimensions describe the resulting desktop source.
Dimensions, positions, rational refresh rates and duplicate identities are
validated before persistence. Windows must validate the supplied configuration
without silently changing requested settings. Success returns `applied:true`
and an opaque lease. A second apply fails `display_busy` until restore completes.

Both restore and recover require the authenticated immediate caller that owns
the journal and its exact lease. Request payloads cannot replace that identity.
Restore works during the original module lifetime; recover uses the same durable
snapshot after a restart or an interrupted operation. Only verified native
restoration followed by durable journal deletion returns `restored:true`.
Recovery never substitutes a guessed monitor, default topology, or fallback mode.
If the saved monitor/source route is unavailable, the journal remains armed.

## Recovery transaction

1. Capture every active classic CCD path, including source modes/positions,
   target signal timings, rotation, scaling, refresh, clone-source association
   and path priority. Perform native `SDC_VALIDATE` without effects.
2. Store the full recovery snapshot, owner and generated lease in
   `physical-display/recovery`, using the state namespace revision CAS. Require
   a successful durable acknowledgement before any native apply.
3. Recheck the captured topology and validate the requested configuration again.
   Immediately before `SetDisplayConfig`, use the runtime's provider-only
   `CheckRequest` to reject cancelled/expired requests.
4. Apply temporary CCD state with `SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_APPLY`;
   force the mode set through the driver. Do not save session settings to the
   Windows persistence database and do not use `SDC_ALLOW_CHANGES`.
5. Query and verify the requested mode/topology. A potentially partial failed
   apply attempts rollback to the durable snapshot. Failed rollback retains the
   journal and reports `recovery_required:true` with the lease.
6. Explicit restore rebinds saved monitor identities to current adapter IDs,
   validates and applies every recorded CCD mode/path, then checks the restored
   state against the snapshot. Clear the journal only after that succeeds.

Snapshot comparison checks ordered monitor/source associations, source sizes,
positions/pixel formats, target signal timings, rotation, scaling and refresh.
Transient adapter addresses and read-only status flags are not identity. Source
GDI routes must still match on recovery; an OS restart that renumbers them may
require manual intervention rather than an unsafe guessed restoration.

The complete journal must fit 12,000 encoded bytes and at most 16 active paths.
Oversized/incomplete snapshots fail before native effects. A crash after the
journal write but before native apply is indistinguishable from an interrupted
apply: recovery is explicit and conservative. Cancellation after the synchronous
native boundary can race with driver work; the journal remains the recovery
source even when a result is lost. A failed journal clear can mean the desktop is
already restored; inspect status and retry recovery. There is no automatic timer,
boot watcher, session-disconnect restore, or administrator takeover API yet.

The lease coordinates callers using this module. It does not prevent the Windows
Settings UI, another process, another independently configured host, or hotplug
from changing the desktop. External changes detected before our apply are not
rolled back. Explicit recovery intentionally restores the saved baseline.

## Native implementation and remaining gates

Windows uses `GetDisplayConfigBufferSizes`, `QueryDisplayConfig`,
`DisplayConfigGetDeviceInfo`, `EnumDisplaySettingsExW`, and `SetDisplayConfig` from
`windows-sys`. All native buffers are owned for the duration of synchronous calls.
Union tags/indexes are validated, and snapshots serialize individual fields;
no native struct padding or raw pointer is persisted.

This implementation uses the classic CCD compatibility view. Active HDR/advanced
color, indirect/virtual/wireless displays, and unsupported/incomplete path data
are rejected for mutation. HDR/WCG state restoration, virtual-mode/virtual-refresh
awareness, DPI scaling, color profiles, driver configuration, primary clone-group
requests, virtual-display drivers, display-stack resets, topology settling and
retry policy remain unimplemented. Existing clone topology can be captured and
restored, but requested clone creation is not supported. Windows service-session,
privilege, display-driver and hardware behavior still require native acceptance.

The behavioral reference was Vibeshine commit
`aa292c4cb097f22beab19d6ebf0307367f9bf449`, its display-helper interfaces and history
concerning physical restore gaps and rotation refresh. No implementation source
was copied or translated. These references motivate capture-before-effects,
verified restore, retained recovery state and forced mode setting; they do not
establish parity with its complete helper.

Validation includes fake-platform transaction tests for state failure before
effects, cancellation at acknowledgement/native boundaries, caller ownership,
restart recovery, failed journal deletion, partial native failure/rollback, and
external changes. Windows native source cross-compiles on the development macOS
host. **No test applies OS display changes on the developer desktop.** This is
not a Windows hardware pass. Hardware acceptance must run an explicitly reviewed
apply/restore/restart-recovery sequence on a disposable Windows test setup with
an independent way to recover the console.

API references: [QueryDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-querydisplayconfig),
[SetDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setdisplayconfig),
[DisplayConfigGetDeviceInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-displayconfiggetdeviceinfo),
[EnumDisplaySettingsExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumdisplaysettingsexw).
