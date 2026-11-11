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
On Windows, an enabled provider using the Sunshine driver applies an explicitly
configured count once during startup. An omitted count leaves the driver alone.
It observes the current count first, skips unchanged values, and independently
reads back a changed count, including after an uncertain acknowledgement.
Status distinguishes applied, failed and unverified effects and exposes the
observed count and any driver error. A failure leaves temporary creation available.
Count confirmation does not establish output activation or capture readiness.
This setting does not change the eight-record limit for temporary leased
monitors.

Windows Create may set `requested_resolution_only:true` to request the driver's
private mode catalog for that exact resolution. The actual opened producer must
advertise control protocol 3.8 or later before this flag is sent. Older producers
continue to support ordinary full-catalog requests and reject an explicit private
catalog without issuing a native Create. Independent driver-state readback must
confirm both the catalog policy and the descriptor dimensions. The policy is
part of the persisted allocation identity; changing its resolution requires owned
removal and recreation rather than retained mode reconfiguration. The caller's
authenticated session policy remains responsible for admitting a private output.
Status exposes `requested_resolution_catalog_supported` as null until a producer
handshake is known, then reports that producer's capability. A source build alone
does not establish driver support or native playback qualification.

`enabled` remains the module's service gate and is independent of the stable
configuration setting names. When it is false, create returns `disabled` without
touching the driver. When the module is enabled, create may manage a temporary
monitor regardless of `dd_activate_virtual_display`. That setting requests
stream-time topology activation; this service does not implement that behavior.
On Windows an explicit create cannot activate topology or make its monitor the
only active display. An `extended` create intent is accepted without changing
peer outputs; the display coordinator activates it through the Display API.
Linux owned creation activates only its new output.
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

Windows monitor identity publication and capture readiness are separate. A ready
monitor can expose its private device path to a trusted display coordinator for
activation while `capture_output` remains absent. The optional `capture_output`
is an opaque `exact-target:` selector shared by the DXGI and WGC Capture SDK
adapters. Its digest does not publish a native target identity or OS handle.
Publication requires the exact owned target to be active at the requested
dimensions, rounded millihertz refresh rate and HDR state, agreement across CCD,
GDI and DXGI enumeration, and unchanged mode/color readback. `ready.capture_error`
explains missing capture readiness. Capture providers independently resolve the
selector to one current target and retain its exact physical identity; malformed,
missing or ambiguous selectors never select the primary output or permit a
geometry fallback. Windows runtime and driver qualification remain separate from
the platform-independent contract and state-machine tests.

`verify_owner_target` lets an explicitly trusted display coordinator verify an
existing caller-owned stable reference without impersonating that caller. Only
principals listed in the provider's trusted startup `target_verifiers` setting
may call it; the default is an empty list. The operation matches the exact owner,
identity and live reference, returning its specification, local target name and
activation state. It grants no creation or release authority and cannot change
the verifier policy. The display coordinator must derive `owner` from its
authenticated inbound broker caller, compare the exact target/specification,
and preserve peer outputs when composing the requested display.

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
Linux `reconfigure` accepts an operation ID, exact caller-owned identity,
reference and borrow, new width, height and refresh, and the expected topology
domain ID and revision. It admits only one owned reference and one provider
borrow. It journals both modes before changing KScreen, then independently
observes the new mode before returning a replacement borrow. Poll the same
operation body with a fresh transport request ID. Once pending has been
reported, a transport error or provider restart is an unknown outcome requiring
reconciliation; startup recovery may remove the journaled connector. The caller
must hold its cooperative mutation reservation and retire capture consumers
before admission, and persist the replacement borrow before publishing capture.
External desktop applications are outside that cooperative fence.
`reconfigure_status` is a recovery-only lookup for the exact caller, operation,
reference, original borrow and domain pair. It never admits an unknown
operation or queues KScreen work. A matching operation can report pending or
its revalidated result; an unknown operation, restart or uncertain effect
returns `recovery_required`.

Linux also exposes `reconfigure_mode_only` and `reconfigure_mode_only_status`
with the same ownership, operation and domain fields. Status advertises the
protocol as `mode_only_preservation: "kscreen-strict-v1"`; this is not proof
that the current desktop exposes all required observations. The operation uses
an already advertised mode and sends only a mode command. It journals the old
and requested mode together with a strict desktop baseline, then verifies the
owned mode and all observed connected-output identities, placement, scale,
rotation, HDR, WCG, priority and VRR policy. Missing evidence fails before the
mode command; an uncertain or mismatched effect retains the recovery fence.
Successful results contain `mode_only_preserved: true`, the verified owned
output's `preservation_readback`, and a replacement borrow. The readback is
observed KScreen state; it does not assert an abstract layout policy.
Each successful report re-queries KScreen and includes equal lowercase SHA-256
`preservation_before_digest` and `preservation_after_digest` values. They cover
the complete connected-output preservation state, serialized as JSON with
recursively sorted object keys and no whitespace; the changing output's mode is
checked separately. A changed or unavailable readback fences the operation and
borrow for recovery. The separate status action only looks up an already
admitted operation. Multiple owned references are allowed when exactly one
provider borrow exists; legacy reconfigure continues to require one reference.

`$XDG_STATE_HOME/amaterasu/virtual-display-linux` (default
`~/.local/state/amaterasu/virtual-display-linux`) is locked against other
Amaterasu hosts and synced before connect. Startup reconciles interrupted owned
connectors before admitting new ones. Persisted nonempty stable references fence
all startup topology changes with `reference_retirement_required`: provider restart
invalidates borrows but does not prove that capture consumers have stopped. Each
original caller must release its exact references after retiring capture. Releases
are persisted individually; only after every recovered reference has been released
does the provider enqueue one cleanup attempt per recovered connector, at most
eight. Status polling never retries failed native cleanup; an explicit owning
`remove` opens another bounded attempt. Readiness returns only after those records
are removed or safely parked. Legacy records without reference metadata retain
their existing best-effort startup cleanup behavior. Failed cleanup retains ownership intent.
When the final reference is released, Linux restores the saved layout before
disconnecting. A monitor that was just enabled does not report its final state
at once, so the layout is read back every 50 ms for up to three seconds and a
difference that outlives 750 ms is applied again, at most three times in all.
A restoration that needed more than one readback writes the output and property
that were late to the service journal.

The disconnect depends on a distinct active output, observed through KScreen
and independently through Wayland for up to two seconds, not on that readback.
With such a survivor the stream output is unplugged even when the saved layout
could not be confirmed, and the layout is applied again afterwards. Only when
no distinct active output can guard the disconnect is an independently observed
owned output parked as a provider-owned desktop anchor that stays connected.
Its released reference is no longer active; the connector and restoration
intent remain journaled and can be reused by a later create, including after
restart. If restoration fails after a confirmed disconnect, a separately
observed surviving output permits the disconnected connector and restoration
intent to be reserved for reuse. Unknown output state or an unresolved
reference is not treated as a successful park.
Reuse configures the new stream from the current desktop rather than requiring
the previously failed restoration to succeed first; the saved recovery layout
remains journaled. If the driver pool was replaced and the old connector no
longer exists, its recovery intent transfers to a free connector in the new pool.

No caller owns a parked output, so the provider retires it itself while no
stream owns a display. Every two seconds it reads connector status from sysfs
alone. When a display outside the virtual pool is attached it repeats the same
restoration and guarded disconnect: one second after parking, at once when the
set of attached displays changes, and otherwise after 2, 5, 10, 30 and then
every 60 seconds. With no such display attached a poll makes no broker request
and runs no compositor command. A parked output that is already unplugged only
awaits its saved layout; that layout is applied at most four more times, after
which it is left to the next stream or a provider restart so that a desktop the
user has since arranged is not replaced. Retirement advances the topology
revision and is recorded in the service journal. This applies to parked outputs
only: a `cleanup_failed` monitor still belongs to its caller and is retried
only by that caller's `remove`.

Vibeshine, the behavioural reference, also requires the surviving monitor to be
listed by its capture backend, requires three consecutive matching readbacks
for final restoration, reacts to monitor power events rather than polling
connector status, and offers a manual action that unplugs every virtual output
at once. These restoration and operator behaviours remain unimplemented here.

KScreen activates the owned connector and selects the requested mode and scale.
Scale can preserve the compositor's choice, use the automatic short-edge
recommendation, or select a supported fixed percentage. All five layouts are
accepted: exclusive, extended, extended primary, and both isolated extended
variants. An exclusive request preserves other owned stream outputs by extending
the desktop. Rejected isolated placement retries adjacent placement while
retaining its mode, scale, HDR and primary choices.

HDR activation first settles the mode and topology with HDR off, then activates
HDR and verifies repeated color and geometry readbacks before publishing
readiness. Restoration has its own bounded verification described above; the
activation checks do not establish restoration stability. KDE can apply a cached single-output layout when a
connector appears. A bounded layout snapshot is synced before each hotplug, and
values displaced by that transition are restored in the same atomic KScreen
operation that configures the new output. Active streams discard the initial
snapshot; removal snapshots the current layout so later user choices are retained.
Interrupted hotplug recovery retains the pending snapshot. Independently
unplugged monitors are never reconnected. Unsupported modes may require KScreen's custom-mode support. Connector disconnection removes
the temporary display; compositor preferences for that connector may persist.
Permanent monitors remain unsupported on this Linux backend. Native KScreen,
driver, display and client qualification remain separate from fixture coverage.

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
effects, bounded cleanup, process exclusion and interrupted recovery. They also
cover a layout that reads back late or must be applied twice, the disconnect
beside a surviving monitor whose layout is unconfirmed, and retirement of a
parked output: its pacing, its silence while no display is attached, its refusal
while a stream is active and its report through the manager. Native
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

On Linux, an identity-specific `status` for a parked output reports
`reference_owned: false`, `parked: true`, and a matching identity and connector
in `parked_outputs`. An idempotent `remove` reports `removed: false` and
`parked: true`. Parking proves release of the stream reference, not connector
disappearance or successful desktop restoration. Parked outputs are excluded
from the active `monitors` list and may be reassigned by a subsequent create.

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

Supported protocol versions are major 3, minor 6 or later. Additive driver
releases preserve the owned display request and response layouts; accepting a
newer minor does not enable its optional operations. This compatibility rule
was checked against Vibeshine `43cf27252a205221a295126a5fcd9ad87ba9c914` and its
driver 3.8 control client. Device interface is
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
at most 8192 paths and 32768 modes, including inactive source/target combinations
reported for virtual adapters, and does not retry a changing topology indefinitely.

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
topology behavior also remains pending. Permanent-monitor startup policy uses the
Windows driver's bounded count operation; other backends report
`permanent_monitor_runtime_supported:false`. Product
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

Retained outputs can change their effective mode and HDR state without replacing
or releasing their allocation. The owning coordinator must retire every capture
consumer first. `prepare_retained_mode` binds a unique operation ID to the exact
identity, owning reference, complete expected reference set and observed
`mode_revision`; unaccounted references and native borrows reject admission.
The provider syncs its old/new intent before a trusted display coordinator may
verify it with `verify_retained_mode` and perform `scoped_reconfigure`. Preparation
keeps the effective specification unchanged and withdraws capture readiness.

`commit_retained_mode` publishes the desired specification only after independent
native mode, HDR and exact target observation. Windows preserves the driver's
immutable allocation specification separately from the effective desktop mode.
Linux serializes preparation and completion through its journal-owning actor and
reads back the exact KScreen output. Neither operation recreates the connector or
changes a reference's ownership. An unsupported advertised mode remains a failure.

After a rejected or rolled-back display request, `abort_retained_mode` can unfreeze
the original source only when the exact old mode and HDR state are independently
observed. Unknown partial effects remain prepared for reconciliation or explicit
owned cleanup. Successful commit and observed abort both advance `mode_revision`,
so a late older operation cannot undo a later transaction. Identical retries are
idempotent and conflicting payloads cannot reuse an operation ID. Status exposes
the effective specification, observation revision and retained-mode transaction;
readiness remains distinct from preparation or local request admission.
