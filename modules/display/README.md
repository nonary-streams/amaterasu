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
or recover. Windows has a native CCD implementation. Linux/KDE has read-only
session enumeration and configuration snapshots; physical display mutation and
macOS display operations remain unsupported.

## Linux/KDE read-only discovery

Linux `enumerate` and `snapshot` query `kscreen-doctor -j` from the module's
existing user session. The query requires that session's
`DBUS_SESSION_BUS_ADDRESS` and absolute `XDG_RUNTIME_DIR`; it never invokes a
shell, requests elevation, or performs a display mutation. Snapshot responses
include normalized active `Configuration` rows, a topology revision, and the
validated KScreen output state needed to preserve session settings such as
scale, rotation, HDR state, priority, and exact mode identity.

`native_supported` remains false for Linux mutations. A source-only restoration
planner has behavioral fixtures for stable output identity, equivalent mode
selection, hotplug, and ambiguous modes. The journal model has a separate,
version-1 KScreen payload; legacy CCD records omit it and continue to decode.
The payload preserves connector identity, mode timing and ID, position, scale,
rotation, enabled/connected and primary state, priority, and optional HDR state.
The existing pipeline validates the complete bounded journal and persists it
through its state revision-CAS boundary before native effects. Linux does not
yet populate this payload in a runtime lease or execute restoration; those
paths remain disabled. This does not qualify KScreen, Plasma, or any
virtual-output driver for runtime use.

## Refresh-rate option model

The library also exposes `RefreshRateOption` as a reusable typed configuration
model. Its default is `Auto`, and its serialized tokens are `auto`, `disabled`,
`manual`, and `prefer_highest`. The legacy token parser maps unknown values to
`Disabled`. The variants describe using client FPS, doing no additional refresh-rate
configuration, using the separate `dd_manual_refresh_rate` setting, or preferring the
highest refresh rate for the selected resolution, respectively.

This model does not persist settings, project scoped values into a runtime
request, or perform provider/native display effects.

## HDR option model

The library exposes `DisplayHdrOption` as a reusable typed configuration model.
Its default is `Auto`, and its exact serialized tokens are `auto` and
`disabled`. The legacy token parser maps unknown values to `Disabled`. The
pinned meanings are:

- `auto` changes display HDR to the state requested by the client when the
  display supports it.
- `disabled` performs no additional HDR configuration.

For `apply`, a policy's `hdr_option` selects HDR for its `display_id`.
`auto` requires `effective_client_hdr_request` (a boolean already resolved by
the caller) and sets that display's requested `hdr_enabled` state. `disabled`
clears the HDR request. A display in the concrete configuration may instead
specify optional `hdr_enabled`: `true` requests HDR on, `false` requests it off,
and omission requests no separate HDR operation. A supplied `hdr_option`
overrides that display's concrete HDR field. The caller must resolve
higher-priority forced HDR/SDR policies before sending the effective request.
This module does not derive client/session facts or project stored global,
application, or client settings into the service request.

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
{"action":"snapshot"}
{"action":"enumerate","offset":0,"limit":16}
{"action":"enumerate","display":"<id returned by enumeration>","offset":0,"limit":32}
{"action":"apply","configuration":{"displays":[{"id":"<id returned by enumeration>","width":1920,"height":1080,"refresh":{"numerator":60000,"denominator":1001},"x":0,"y":0,"rotation":"identity"}]}}
{"action":"apply","configuration":{"displays":[{"id":"<id returned by enumeration>","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0}]},"mode_policy":{"display_id":"<same id>","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false},"hdr_option":"auto","effective_client_hdr_request":true}}
{"action":"apply","configuration":{"displays":[{"id":"<first id>","width":1920,"height":1080,"refresh":{"numerator":60,"denominator":1},"x":0,"y":0},{"id":"<second id>","width":1280,"height":720,"refresh":{"numerator":60,"denominator":1},"x":1920,"y":0}]},"mode_policies":[{"display_id":"<first id>","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}},{"display_id":"<second id>","remapping":{},"context":{"resolution_is_automatic":true,"refresh_rate_is_automatic":true,"client_display_mode_override":false,"optimize_game_settings":false}}]}
{"action":"restore","lease":"display-0000000000000001"}
{"action":"recover","lease":"display-0000000000000001"}
```

`snapshot` returns the complete active CCD configuration and an opaque
`topology_revision` from a validated active-path set. It rejects incomplete
paths and ambiguous monitor IDs. Pass that revision as
`expected_topology_revision` on Apply to require the same active topology at
durable prepare and the native apply boundary. The token covers the active CCD
state, including clone path priority. Reordering independent
nonclone paths does not invalidate the token. Clients should request a fresh
snapshot after updating the provider; older clone tokens remain valid recovery
journal data but cannot authorize a new Apply. A revisioned Apply selects
exactly the currently active ID set, so an omitted monitor cannot be disabled
by a partial policy. Legacy Apply still permits validated inactive target
activation. Hotplug or display changes return `topology_changed` before the
journal or native mutation when detected at prepare.

Apply can carry a stable ASCII `operation_id` (1–128 graphic bytes). It is
persisted with the recovery journal before native mutation and returned in
owner-visible Status with the lease. An exact retry with the same owner, ID,
original configuration, expected revision, and mode policy returns the existing
lease as `recovery_required` with `applied:false`, without resolving modes or
repeating a native effect. A changed request using the same ID conflicts.
Older journals without the original request identity retain exact no-policy
retries against their saved concrete configuration and expected revision.
Policy retries against those journals fail closed; their owner can recover or
restore the saved lease.

A single `mode_policy` may include `configuration_option`. `disabled` returns
without a state-service request or native effect. `verify_only` reads the
coherent active topology and requires its selected display to be active;
neither option resolves mode/HDR policy, creates a lease, or requires mutation
startup permission. Mutating options require `expected_topology_revision` and
use the durable Apply/restore journal. `ensure_only_display` applies the
selected target at the origin and disables all other active paths. An inactive
physical target needs one unambiguous free QDC route and a restorable baseline.
Every path it deactivates must have a queryable saved HDR state so recovery can
restore the complete baseline.
`ensure_active` and `ensure_primary` accept an already active selected target.
On a multi-monitor desktop they rebuild only that target's requested mode and
translate untouched source positions for a primary change, while retaining the
saved source/target modes, timing, scaling, pixel format, and route flags of
other targets. Full native readback verifies those untouched paths after HDR.
When the selected target shares an active clone source, its resolved resolution
and source position apply to the whole clone group. The provider preserves each
peer's target refresh and timing, places the selected target first in CCD path
priority for `ensure_primary`, and journals the exact prior clone topology.
Windows CCD readback must return that target first in path priority order after
Apply and after HDR changes. Clone restoration also verifies the saved clone
priority before releasing the recovery lease.

`ensure_active` and `ensure_primary` can also activate an inactive physical
target when QDC reports
exactly one available free source route for one physical target. The provider
persists that monitor/source/target binding with the journal and rechecks it
after journal acknowledgement and just before native Apply. `ensure_active`
preserves all previously active paths and places the new target to their right.
`ensure_primary` places the selected target at (0,0), then translates the old
desktop with one checked x/y offset so its leftmost path touches the selected
target. The new primary takes first CCD path priority; old paths retain their
relative priority and full native modes. Native readback must prove the entire
requested layout or restore the saved baseline. Windows hardware may reject or
rearrange layouts that fail its desktop connectivity rules. HDR=true is
rejected without a saved baseline; false or unspecified HDR must read back as
off after activation, or the provider rolls back.
The selected path uses the existing CCD Apply contract, including its requested
32bpp mode and preferred scaling.

`status` returns native support, startup mutation policy, `state` (`idle`, `leased`
or `recovery_required`), `owned_by_caller`, and the lease and operation ID only to its owner. After a
module restart every outstanding journal is reported as requiring recovery.

Enumeration without `display` returns connected monitor identities, friendly
names, active-source/current-mode data and advanced-color status (null when the
OS cannot report it). `id` is the lowercased Windows monitor device-interface
path, not a transient adapter LUID. Driver reinstallation or a connection change
can replace that identity; it is not a universal portable monitor UUID.

Enumeration with `display` returns GDI modes for that **active** source. These
records expose integer nominal `refresh_hz`. The active CCD mode contributes its
exact rational `refresh` only when a matching 32bpp GDI mode exists; other GDI
modes retain their integer nominal rates. `prefer_highest` considers only 32bpp
modes, prefers the exact current rate when nominal rates tie, and requires native
CCD validation before journaling. Drivers may report fractional rates only as
rounded integers, so selection on other modes needs hardware qualification.
CCD apply validates the exact request with Windows. Inactive-target mode enumeration is
explicitly unsupported. Results are `entries` and `next_offset`; limits are 1–32
and pages also cap encoded rows at 12,000 bytes. Offsets are not immutable
snapshot cursors; restart enumeration after hotplug or external display changes.
Native buffer counts and driver mode enumeration have finite admission limits.

Apply specifies the **entire desired active display set**. Omitted displays are
disabled. Different source groups use disjoint rectangles; cloned targets occupy
the same rectangle and share one CCD source. One display must begin at `(0,0)`.
Rotation is `identity`, `rotate90`, `rotate180`, or `rotate270`; dimensions
describe the resulting desktop source.
The optional `mode_policy` retains the single-display wire format. Use optional
`mode_policies` to resolve several display policies in one configuration. Supplying
both fields is an `invalid_request`, even when the list is empty. Policy targets
must be distinct ignoring case and present in `configuration.displays`. Every
policy resolves its own display, including per-target `prefer_highest` mode lookup;
the final complete configuration is validated before one journaled prepare/apply
transaction. An empty list applies the concrete configuration as given.
Dimensions, positions, rational refresh rates and duplicate identities are
validated before persistence. Windows must validate the supplied configuration
without silently changing requested settings. Success returns `applied:true`
and an opaque lease. A different apply fails `display_busy` until restore completes;
an exact same-owner operation ID retry returns the existing lease with
`recovery_required` and `applied:false` until recovery verifies native state.
On Windows, HDR-on requires a currently active target with queryable HDR
support. HDR-off is a no-op on an active target without HDR support. An
inactive target may be activated with HDR-off requested: its HDR state is
checked after CCD activation without changing an unjournaled HDR baseline.
If it comes up HDR-on, apply fails and rolls back. This service reports
`applied:true` only after CCD and requested HDR state verification. The requested
mode, topology, and every requested HDR state are read back again after the
final HDR operation because an HDR change may affect another target or cause
the driver to renegotiate a mode. A late mismatch fails
apply and enters the journaled rollback path.

Both restore and recover require the authenticated immediate caller that owns
the journal and its exact lease. Request payloads cannot replace that identity.
Restore works during the original module lifetime; recover uses the same durable
snapshot after a restart or an interrupted operation. Only verified native
restoration followed by durable journal deletion returns `restored:true`.
Recovery never substitutes a guessed monitor, default topology, or fallback mode.
If the saved monitor/source route is unavailable, the journal remains armed.

## Recovery transaction

1. Capture every active classic CCD path, including source modes/positions,
   target signal timings, rotation, scaling, refresh, clone-source association,
   path priority, and queryable per-target HDR state. Perform native
   `SDC_VALIDATE` without effects for CCD-changing requests.
2. Store the full recovery snapshot, owner and generated lease in
   `physical-display/recovery`, using the state namespace revision CAS. Require
   a successful durable acknowledgement before any native apply.
3. Recheck the captured topology and validate the requested configuration again.
   Immediately before `SetDisplayConfig`, use the runtime's provider-only
   `CheckRequest` to reject cancelled/expired requests.
4. Apply temporary CCD state with `SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_APPLY`;
   force the mode set through the driver. A request changing only one active,
   queryable HDR state skips both CCD validation and apply when every requested
   display mode, position, rotation and clone association already matches the
   durable snapshot. This path preserves the existing native pixel format,
   signal timing, scaling, scan ordering and route; it requires 32bpp active
   paths and does not handle activation or route changes. Do not save session
   settings to the Windows persistence database and do not use
   `SDC_ALLOW_CHANGES`.
5. Query and verify the requested mode/topology. Apply any HDR request with
   `DisplayConfigSetDeviceInfo` and read back its state, then verify the requested
   mode/topology again. The HDR-only path checks the entire resulting snapshot
   against its baseline with only the requested HDR bit changed. A potentially partial
   failed apply attempts rollback to the durable snapshot. Failed rollback
   retains the journal and reports `recovery_required:true` with the lease.
6. Explicit restore rebinds saved monitor identities to current adapter IDs,
   validates and applies every recorded CCD mode/path, then checks the restored
   mode and saved HDR state against the snapshot. For older SDR journals without
   HDR fields, recovery blocks before mutation if a saved target is inactive
   or now has HDR enabled. Clear the journal only after verification succeeds.

Snapshot comparison checks ordered monitor/source associations, source sizes,
positions/pixel formats, target signal timings, rotation, scaling, refresh and
recorded HDR state.
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
`DisplayConfigGetDeviceInfo`, `DisplayConfigSetDeviceInfo`,
`EnumDisplaySettingsExW`, and `SetDisplayConfig` from
`windows-sys`. All native buffers are owned for the duration of synchronous calls.
Union tags/indexes are validated, and snapshots serialize individual fields;
no native struct padding or raw pointer is persisted.
Restore readback compares saved mode, placement, scaling, path flags,
output technology, and queryable HDR state. Transient adapter LUIDs are
rebound after restart rather than compared as persistent identities. A driver
that normalizes these saved flags can leave the recovery lease armed until its
behavior is qualified; this has not been tested on physical display hardware.
Readback matches paths by monitor identity independent of array order, except
for selected primary priority and restoration of existing clone path priority.
Read-only source/target status flags may drift and are not restore invariants.

This implementation uses the classic CCD compatibility view and journals the
queryable HDR on/off state of active paths. Active HDR can be restored; enforced
wide-color-gamut state, indirect/virtual/wireless displays, and unsupported or
incomplete path data remain outside this provider. Virtual-mode/virtual-refresh
awareness, DPI scaling, color profiles, driver configuration, virtual-display
drivers, display-stack resets, and broader topology transition retries remain
unimplemented. After native apply or restore, bounded CCD readbacks wait for the
exact requested state and reject foreign physical routes immediately. Existing
clone sources can be remapped and restored while keeping each target's refresh,
rotation and HDR state. An active clone target can become primary while retaining
its shared source and peer target state. A revisioned Apply can add one inactive
physical target as a clone of an active source when its route is uniquely bound
and journaled; detected route drift fails before native mutation. Arbitrary
clone-group reconfiguration remains unsupported. Windows service-session,
privilege, display-driver and HDR hardware behavior still require native acceptance.

The behavioral reference was Vibeshine commit
`aa292c4cb097f22beab19d6ebf0307367f9bf449`, its display-helper interfaces and history
concerning physical restore gaps and rotation refresh. No implementation source
was copied or translated. These references motivate capture-before-effects,
verified restore, retained recovery state and forced mode setting; they do not
establish parity with its complete helper.
The HDR option behavior was also checked against pinned Vibeshine commit
`457237db7e57f9c4be21c35bb07e28d1c100eb7d`; its higher-priority forced
states and effective client HDR decision still require caller integration here.

Validation includes fake-platform transaction tests for state failure before
effects, cancellation at acknowledgement/native boundaries, caller ownership,
restart recovery, failed journal deletion, partial native failure/rollback, and
external changes, HDR enable/disable/readback, unsupported and inactive targets,
rollback, and legacy-journal safety. The Windows native source and strict Clippy
pass on the development Windows host. Deterministic native-translation tests
cover delayed CCD convergence, a permanent mismatch, and changed target routes
during restoration. **No test applies OS display changes on
the developer desktop.** This is not an HDR hardware pass. Hardware acceptance
must run an explicitly reviewed
apply/restore/restart-recovery sequence on a disposable Windows test setup with
an independent way to recover the console.

API references: [QueryDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-querydisplayconfig),
[SetDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setdisplayconfig),
[DisplayConfigGetDeviceInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-displayconfiggetdeviceinfo),
[DisplayConfigSetDeviceInfo](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-displayconfigsetdeviceinfo),
[EnumDisplaySettingsExW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumdisplaysettingsexw).
