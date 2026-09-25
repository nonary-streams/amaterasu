# Frame limiter service and policy

This crate defines typed global frame-limiter settings, inventory-scoped
override resolution, provider policy, and the managed
`amaterasu.frame-limiter/1` service.

The version 1 `apply` request carries the immutable effective settings for one
launch plus optional stream, Lossless Scaling, explicit client display-mode,
virtual-display, and frame-generation context. Missing or null context is
unknown; the launch adapter must not infer a client display override from
stream dimensions. Rate values are exact unsigned millihertz, and zero means
the known follow-stream/no-rate value. The launch adapter resolves the six
application/client overridable fields with client-over-application-over-global
precedence. Other settings remain global. The legacy
`rtss_disable_vsync_ullm` input folds into `frame_limiter_disable_vsync`, and
callers persist only the canonical field. An empty RTSS install path selects
auto-detection.

## Runtime behavior

64-bit Windows `apply` supports RTSS when the request selects `rtss` or `auto`,
policy requires a limiter, the limit is known, RTSS is running, and its profile
API is available. The module finds the running `RTSS.exe`, loads the sibling
`RTSSHooks64.dll` from that installation, and applies the exact rational FPS
limit, denominator, and sync-limiter value to the executable profile. It reads
all three values back before returning `applied: true` and a lease. The
`SyncLimiter` numeric mapping follows the RTSS community interface; RTSS does
not publish a stable vendor API contract for this property. `auto` has no
unimplemented fallback: if RTSS cannot be used, the response is
`runtime_unavailable`. Other providers, 32-bit Windows, and non-Windows RTSS
requests also remain unavailable.

The RTSS limiter-disabled flag is global, so the module only reads it. If RTSS
has disabled the limiter globally, apply reports unavailable without changing
the flag or other applications' profiles. RTSS cannot apply the separate
`frame_limiter_disable_vsync` setting, so requests that enable it are declined
before any provider mutation. Settings that cannot be read and restored exactly
are also declined. An explicit relative RTSS install path is accepted by
settings validation but currently returns unavailable; leave the field empty
for auto-detection or supply an absolute path to the running installation.

A lease belongs to the broker caller and `session_id`. An exact request retry
returns the same lease while RTSS still matches, and the retry check runs
before policy can return `not_needed` or `runtime_unavailable`. Changed
settings or target for that owner and session return `lease_conflict`, including
after restore while its tombstone is retained. Conflicts are scoped to one RTSS
installation and executable profile, so independent profiles may have active
leases at the same time. A stale lease continues to reserve its profile until
it is restored or resolved. `status` lists only that caller's leases and may
filter by session. When no owned lease exists, `status` probes the RTSS process
and required API and reports provider availability separately from lease state.

`restore` accepts exactly one `session_id` or lease token. For an existing
profile it restores only the three limiter properties owned by the lease,
preserving other profile settings. If apply created the profile, the module
stores its full profile-file bytes (up to 64 KiB) and compares them before
deleting the whole profile. If the bytes changed, or a restart finds a created
profile without a durable byte snapshot, status reports a stale lease and
restore leaves the profile in place. For a profile that was absent before
apply, restore verifies that it is absent and that the global limiter flag is
unchanged; inherited Global profile values may change independently. RTSS has
no conditional delete API, so an external writer could still race the final
byte comparison and profile deletion. A changed state detected before restore
is never overwritten. The journal keeps the latest 256 completed lease
tombstones; a repeated restore returns `restored` while its tombstone remains,
then returns `not_found` after eviction. Callers should use unique session IDs.

The lease journal is stored under the current user's `LOCALAPPDATA` (falling
back to `APPDATA`) in `Amaterasu/frame-limiter/leases.json`. The runtime API's
state-root grant is restricted to the state module. Before an RTSS mutation,
the module atomically persists a pending lease with its exact before and
intended values. On startup it reconciles that record with RTSS: an exact
applied state becomes active, an exact before-state is finalized as no effect,
and a third state remains stale for owner-visible recovery. A created profile
is promoted only when its full file snapshot was durably saved; a crash between
profile creation and that save is explicitly reported as stale. A readback
mismatch is rolled back only when the current profile and global limiter flag
still match the attempted change and, for a lease-created profile, its exact
byte snapshot.

## Policy behavior

The Windows RTSS policy changes the configured sync type only for an active
frame-generation policy with Auto or RTSS selected. Smooth Motion uses Front
Edge Sync. Automatic virtual-display policy selects Reflex only when enabled,
the global override is off, and NVIDIA is present without an AMD adapter.
Active game-provided frame generation selects Reflex on supported NVIDIA
hardware even when the virtual-display automatic mode or global override is
off; an explicit application/client RTSS type takes priority. A zero configured
FPS limit uses, in order, a Lossless Scaling limit, explicit client display-mode
rate, and authoritative requested-stream FPS. When those sources are known to
be absent or zero, the result remains `FollowStream`.

## Remaining integration and platform gates

The frame-limiter service now has an RTSS runtime, but Team 2 still needs to
call apply with the frozen launch snapshot, persist the returned lease with its
session, and retry restore during stop. No Team 2 files were changed in this
slice. Live RTSS DLL property readback, save/update behavior, and hardware
restore were not exercised because RTSS is not installed and running in the
build environment. RTSS hooks may also be blocked by a game's anti-cheat. The
community `SyncLimiter` numeric mapping and the RTSS API's real save/update and
restore behavior still require verification against supported RTSS versions.
NVIDIA Control Panel, VSync control, MangoHUD, and Proton runtime providers
remain unimplemented; unsupported provider choices return `runtime_unavailable`.

Focused policy and lifecycle tests are in `src/tests.rs` and `src/lease.rs`;
strict service wire tests are inline in `src/service.rs`.
