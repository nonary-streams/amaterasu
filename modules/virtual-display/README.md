# Virtual display adapter

`amaterasu_virtual_display` is an independently built native library providing
`amaterasu.virtual-display/1`. It implements the Sunshine Windows virtual display
control protocol with temporary owned leases. It does not install or modify a
driver. Settings default to
`{"enabled":false,"dd_activate_virtual_display":false,"dd_use_sunshine_virtual_display_driver":true}`.
The permanent-monitor setting `dd_virtual_display_permanent_count` has a range
of 0 through 4; the legacy name `dd_vdd_static_monitor_count` is also accepted,
and the canonical name takes precedence if both are supplied. Integral values
outside the range normalize to zero while remaining configured, matching
Vibeshine. Non-integer and null values are rejected. Status reports numeric zero
for an unset value and uses a separate configured flag to distinguish it from
explicit zero.
This setting does not change the eight-record limit for temporary leased
monitors.
`enabled` remains the module's service gate and is independent of the stable
configuration setting names. When it is false, create returns `disabled` without
touching the driver. When the module is enabled, create may manage a temporary
monitor regardless of `dd_activate_virtual_display`. That setting requests
stream-time topology activation; this service does not implement that behavior.
On Windows an explicit create cannot activate topology or make its monitor the
only active display. Linux owned creation activates only its new output.
If `dd_use_sunshine_virtual_display_driver` is false, create returns
`unsupported_driver`; this service has no alternative driver and does not silently
select Sunshine. Builds without the optional Linux backend return
`unsupported_platform` outside Windows.

The provider needs only its service registration grant; clients need its call
grant. On Windows, an enabled provider stores a private lease journal beneath
`LOCALAPPDATA/Amaterasu/virtual-display` (falling back to `APPDATA`). It fails
startup when this location is unavailable, linked, corrupt, or inaccessible.
There are no state-service or subprocess dependencies.
`serde`/`serde_json` implement the bounded SDK JSON boundary, `sha2` derives stable
caller-specific IDs, `getrandom` supplies lease capabilities, and `windows-sys`
provides documented Win32 bindings. Native code remains trusted, not sandboxed.

## API

The optional Linux `linux-libvirtualdisplay` feature directly links the pinned
`third-party/libvirtualdisplay` CMake driver target. Initialize that submodule
before building. `linux_control::LinuxControlClient` calls the library's original
C++ broker client through a small exception-safe C ABI; Amaterasu does not
reimplement its wire protocol. The default endpoint remains
`/run/vibeshine/vkms-control.sock`, where the installed broker enforces Unix UID
connector ownership shared with Vibeshine. Query does not grant ownership, and
set always reaches the broker even when the observed state already matches.

With that feature, Linux create/remove use a separate monitor controller. A
bounded actor serializes native effects outside the input/media tick. Each
caller/UUID gets the lowest free connector the vibeshine DRM card actually
exposes; already connected outputs are never adopted. DRM connector type ids are
global and are not reset when the driver reloads, so a reloaded pool is named
Virtual-9 through Virtual-16 rather than Virtual-1 through Virtual-8. The actor
therefore lists `/sys/class/drm/card<N>-Virtual-<M>` entries whose card device
is bound to a `vibeshine` or `vibeshine_drm` driver, or is the `vibeshine` faux
device (stock vkms, also a faux device, is excluded). It reads sysfs metadata
only and never opens a DRM node. Missing sysfs, no vibeshine connectors, or an
id present on two vibeshine cards fails the create before any journal entry or
broker effect. The broker accepts any `Virtual-<id>` public name (a positive
32-bit id) and resolves it through the driver's `drm_name` mapping. Journal
records accept the same names. The shared broker rechecks Unix UID
ownership on every mutation. The private user journal at
`$XDG_STATE_HOME/amaterasu/virtual-display-linux` (default
`~/.local/state/amaterasu/virtual-display-linux`) is locked against other
Amaterasu hosts and synced before connect. Startup removes interrupted owned
connectors before admitting new ones. Failed cleanup retains ownership intent.

KScreen activates only the new connector, selects the requested mode, sets 100%
scale and SDR, and places it to the right of enabled outputs. Readback must match
before status reports `ready`. KDE can apply a cached single-output layout when a
connector appears. A bounded layout snapshot is synced before each hotplug, and
values displaced by that transition are restored in the same atomic KScreen
operation that configures the new output. Active streams discard the initial
snapshot; removal snapshots the current layout so later user choices are retained.
Interrupted hotplug recovery retains the pending snapshot. Independently
unplugged monitors are never reconnected. Unsupported modes may require KScreen's custom-mode support. Connector disconnection removes
the temporary display; compositor preferences for that connector may persist.
Nonzero scale requests and permanent monitors remain unsupported on this backend.

The Linux session profile's `--enable-virtual-display` selects
`amaterasu-owned-display`. The stream coordinator derives an owned identity,
creates the display before opening video, and routes capture to the returned
`kwin:Virtual-N` output. Input receives current logical desktop geometry through
the video v1.1 metadata tail. Stop closes video/capture before removing the owned
connector; interruption retains journaled recovery. Native-module unload also
performs best-effort exact cleanup. This is a shared KDE session with separate
outputs, not an independent login or native-code sandbox.

Build capture with `amaterasu-capture-pipewire/linux-kwin-screencast` as well.
KDE must authorize the exact host executable's ScreenCast interface through its
scoped desktop declaration. The service bundle emits this declaration and an
installer; no global KWin permission check is disabled. The desktop user must
also have access to the existing broker socket. A socket ACL must be reapplied
on socket recreation, for example through an administrator-owned socket unit
drop-in. Do not replace the broker, unload the driver, or steal Vibeshine's output.

Tests cover sysfs connector discovery against a fake tree (reloaded ids,
non-vibeshine and stock vkms cards, malformed names, ambiguity), free-connector
allocation past Virtual-8, caller identity, durable intent before
effects, bounded cleanup, process exclusion and interrupted recovery. Native
qualification must separately exercise actual display creation, capture, input,
playback and restoration. Private DRM frame export / GPU zero-copy interop and
HDR capture are not implemented by this integration.

Requests reject unknown fields. Examples:

```json
{"action":"create","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09","width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu"}
{"action":"status"}
{"action":"status","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09"}
{"action":"remove","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09"}
```

Lifecycle callers may attach a stable `reference_id` to `create`, query that
reference with `status`, and release only it with `remove`:

```json
{"action":"create","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09","width":1920,"height":1080,"refresh_millihz":60000,"name":"Amaterasu","reference_id":"stream-session-42"}
{"action":"status","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09","reference_id":"stream-session-42"}
{"action":"remove","identity":"9c1d707f-7229-4fc4-b6e2-91f80c626a09","reference_id":"stream-session-42"}
```

Reference IDs are caller-scoped, 1–128 printable ASCII characters, and are
persisted with the lease journal. Repeating a create with the same caller,
identity, specification, and reference is idempotent. A second reference with
the same specification shares the temporary monitor without replaying native
creation. Releasing a reference leaves the monitor alive while another
reference remains; releasing the final reference begins exact native cleanup.
Status reports `reference_owned` for a queried reference and never returns the
reference token. A remove without `reference_id` preserves caller-wide legacy
removal. Legacy and reference-counted ownership cannot be mixed for one
identity. If completion of a newly attached reference is lost, only that new
reference is released; a previously owned monitor and its other references
remain intact. Restart resumes either reference ownership or final cleanup from
the journal.

Create may include the optional integer `dd_virtual_display_scale`; omission
defaults to `0` and preserves older callers. Accepted values are `-1`, `0`,
`100`, `125`, `150`, `175`, `200`, `225`, `250`, `300`, `350`, `400`, `450`,
and `500`. Zero leaves the current compositor scale unchanged because this
module applies no scale effect; retained-factor lookup is not implemented.
`-1` requests automatic scaling, and a positive value requests a fixed
percentage without changing output-pixel resolution. Automatic target
resolution and application of automatic or fixed requests are not implemented
by this module. Valid nonzero requests return `unsupported_scale` before any
driver or native call. Zero and omission continue through the existing
temporary-monitor create behavior. Non-integer JSON values and integers outside
the accepted set return `invalid_request`.

Identity must be a nonzero canonical lowercase UUID. Resolution bounds are
320–7680 by 200–4320; refresh is 1000–1000000 millihertz. Name is 1–31 printable
ASCII characters. Physical size is currently fixed at 600 by 340 mm, with default
driver HDR luminance. At most eight records exist across callers.

Create returns `{"ok":true,"monitor":{...}}` when ownership is recorded, including
when a failed driver acknowledgement starts cleanup. Inspect `state` and `error`:
`creating`, `ready`, `recovered`, `reclaim_pending`, `removing`, or `cleanup_failed`.
`recovered` is a capability-verified lease whose original CCD binding may be
unavailable after a lost create reply; it remains fed but does not advertise
capture readiness. `reclaim_pending` blocks feed and removal until exact driver
state resolves an interrupted reclaim. A `ready` value contains the
validated Windows monitor `device_path` and whether CCD reports the path active.
Readiness means the exact returned adapter/target has a published monitor identity;
it does not mean capture, encoding, streaming, or topology activation is ready.
Creation waits up to five seconds for that identity before cleanup starts.
If a published identity disappears while its lease still exists, status immediately
clears the stale device path and starts a new five-second readiness window. The
same owned lease may become ready again; otherwise exact cleanup begins.
If the exact lease is already pending departure, readiness is revoked and cleanup
starts immediately; the manager does not keep feeding a departing lease.

Each caller sees/removes only its own records. Repeating create with identical
caller, identity, and specification returns the existing record without replaying
the native effect. Changing its specification returns `identity_conflict` until
removed. A SHA-256 derivation of caller and UUID yields the stable requested driver
ID; a fresh random lease/capability is used for each new lifecycle. Neither secret
is exposed. The journal durably records owner, specification, capability, and
lease before native creation. Restart reclaims a live matching monitor through
the driver's capability-checked fencing operation, which rotates to a fresh
lease. The prospective lease is journaled before that operation; a lost reply
is resolved by inspecting both exact IDs. A stale capability or mismatched
driver specification never authorizes feed or removal. Missing remove is idempotent. A remove of another caller's UUID never
removes that caller's monitor. Status is a bounded snapshot, not persistent history;
it reports the module gate and both canonical setting names, marks activation as
unsupported through `activation_supported`, `activation_state`, and
`activation_error`, and identifies the driver as `unsupported` when the Sunshine
driver is not selected.

## Protocol provenance and safety

Behavior and ABI were inspected at Vibeshine commit
`457237db`, whose pinned `third-party/libvirtualdisplay` commit is
`3083d210b55fba4b886e5c634e6abee0942201d4`, particularly
`src/driver/include/virtual_display/driver/control_protocol.h`. This adapter is an
original implementation; it does not copy or translate the reference implementation.

Supported protocol versions are exactly major 3, minor 6 or 7. Device interface is
`5f894d6c-3a69-48a2-86ef-e4c671932d63`; protocol namespace is
`a2864284-77fe-4336-a828-00feec89ebac`. IOCTLs use buffered FILE_DEVICE_UNKNOWN
control codes with these function numbers and byte layouts:

| Operation | Function | Input / output bytes |
| --- | --- | --- |
| Version | 0x900 | 0 / 24 |
| Owned temporary create | 0x90c | 128 / 56 |
| Capability-checked reclaim | 0x90d | 72 / 48 |
| Exact lease/display remove | 0x902 | 32 / 0 |
| Lease feed | 0x903 | 32 / 0 |
| Lease query | 0x905 | 32 / 48 |
| State query | 0x908 | 0 / 1696 |

The byte codec explicitly encodes little-endian fields instead of relying on Rust
layout. Reply size, namespace, version, IDs and counts are checked before use.
Device and monitor paths require valid terminated UTF-16. Interface enumeration is
bounded and fails on ambiguous multiple driver interfaces. CCD enumeration allows
at most 256 paths/modes and does not retry a changing topology indefinitely.

IOCTLs use overlapped I/O with a 200 ms event timeout and exact-operation
cancellation. Cancellation is drained before releasing native buffers. A defective
driver that never completes cancellation can still block that drain; synchronous
CreateFile/device enumeration/CCD calls likewise have no module-level interrupt.
The isolated host worker watchdog is the final recovery boundary for these hangs.
No background thread can outlive this DLL. These limits are not a real-time promise.

A ten-second lease is fed once per second while healthy. One monitor is maintained
per tick, with 200 ms polling. Ambiguous creation is never replayed: it enters exact
lease/display cleanup. Cleanup makes at most three native removal attempts per
window; an explicit Remove by the owning caller can retry a failed window. The
manager keeps its record and journal entry until state and pending-departure
queries confirm removal. Unresolved restart records block duplicate creation.
Failed cleanup stops lease feeding. Drop makes one best-effort exact removal per owned record;
there is no remove-all command. A newly admitted create whose completion is cancelled or cannot be delivered enters
exact cleanup and stops feeding; failed completion of an idempotent retry leaves
the previously admitted monitor intact. Crash/cancellation after create can leave a
temporary monitor until cleanup or driver lease expiry; driver failure can delay
actual OS departure.

## Remaining release gates

Real Windows hardware/driver qualification remains required; tests use a fake backend
and byte fixtures, and cross-compilation does not prove driver interoperability.
The driver does not return the CCD adapter/target binding on reclaim. A lease
recovered from a lost create reply therefore remains in `recovered` state and
cannot report a ready device path until a future API provides that binding.
If the driver is unavailable or the lease has expired, recovery waits for
confirmed exact departure; it does not issue a duplicate create. Driver identity
retention/cleanup policy, persistent monitor manifests, adapter selection, HDR/ICC,
DPI controls, and the incompatible SudoVDA protocol are not supported. Linux per-stream ownership is described above. Application window routing,
private DRM frame transport, persistent monitors and independent KDE logins
remain outside that integration.

Tests exercise setting defaults and separation, activation diagnostics and driver
selection, caller isolation, stable identity/idempotency, readiness/feeding,
ambiguous creation, bounded exact cleanup, departure confirmation, and malformed protocol
lengths/counts/UTF-16. Persistent global configuration projection and per-client
behavior remain pending; the defaults and module settings payload do not provide
that configuration plumbing. The `dd_activate_virtual_display` stream-time
topology behavior also remains pending. Permanent-monitor creation is unsupported:
status reports the configured count and `permanent_monitor_runtime_supported:false`,
but this module does not ask the driver to create permanent monitors. Product
settings persistence and migration, and management edit authority remain pending.
Windows driver qualification for permanent monitors has not been run. Run `cargo test -p
amaterasu-virtual-display` and cross-check with `cargo check -p
amaterasu-virtual-display --target x86_64-pc-windows-msvc`.

### Cooperative Linux capture binding

The optional Linux backend exposes `borrow`, `validate_borrow`, and
`release_borrow` on this service. Each takes `identity` and `reference_id`;
validation and release also take the provider-generated `borrow_id`. Borrow
requires an existing ready caller-owned reference and independently observes the
current `wl_output` global, name and current mode through a persistent Wayland
connection. It also checks broker connection state. The response includes
`observed_output` with version 1, protocol `wayland-wl-output`, registry global,
output name, width, height, refresh in millihertz and `compositor_id`, a digest
of the Wayland peer process identity and boot ID. Both connections must be in
the same PID namespace and reach the same compositor rather than separate
proxies. The consumer must compare
this tuple with its own live observation and validate before and after opening
capture and throughout its lifetime. Changed mode, removal, observer failure,
provider restart, or an unavailable observation invalidates validation. An invalid borrow continues to block releasing its reference until the consumer
explicitly releases it after retiring capture. A borrow blocks releasing its reference, and caller ownership is checked on every
operation. Lost borrow completion releases the newly created borrow.

This is a cooperative fence against this provider's own removal and reuse, not
an OS-enforced connector lease. The broker's read-only query reports connected
state, not owning UID or an allocation nonce. Wayland globals are scoped to the
compositor, so producer and consumer must use the same compositor. External
replacement may occur between validations; consumers must stop on invalidation.
A native binding to a privileged or uncooperative actor is not established.
Windows reports `unsupported_binding` for these operations.

### Linux layout and cooperative mutation domain

`create` accepts optional `virtual_display_layout` (`exclusive` or `extended`)
and the pair `expected_topology_domain_id` and `expected_topology_revision`. Omitting the layout retains the configured
module default. The requested layout and scale are part of the active identity:
reusing the same caller/UUID with different values returns `identity_conflict`.
Unsupported layouts are rejected. This does not reconfigure module settings.

Linux status returns `settings` and `mutation_domain`, which has an opaque
provider-lifetime `id`, numeric `revision`, `borrowers`, `pending_mutations`, and
`scope: "provider_session"`. Pass the exact domain ID and numeric revision together as
`expected_topology_domain_id` and `expected_topology_revision` to reject a stale create. The random domain ID
changes on provider restart. The revision advances on admitted native topology
operations and borrow acquisition or release. A new create or final output
removal is rejected while any borrow exists; acquiring a borrow is rejected
while a provider native mutation is queued or running. Idempotent reuse and
non-final reference releases do not require a native topology mutation. A
caller must release its borrow after capture is retired before removal.

This domain covers only changes admitted by this provider, not external KScreen
or broker effects. The independent compositor/output observation and continuing
validation remain necessary. Windows rejects explicit per-request layout or
revision with `unsupported_topology_domain`; its prior requests remain valid.
