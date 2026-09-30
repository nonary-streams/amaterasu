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

64-bit Windows can detect a running RTSS process and inspect profile values
through `RTSSHooks64.dll`, but it cannot safely mutate those profiles through
the available API. The RTSS profile interface separates `SetProfileProperty`
from an unconditional, void `SaveProfile` call and offers no compare-and-save
operation or lock shared with the RTSS UI. An external writer could change the
profile after a comparison and have that change overwritten by the save. The
module therefore leaves those mutation calls unbound. A policy-required RTSS
apply returns `runtime_unavailable` with
`rtss_conditional_profile_write_unsupported`, `applied: false`, and no lease.
Other providers, 32-bit Windows, and non-Windows RTSS requests also remain
unavailable. `auto` has no unimplemented fallback.

The RTSS limiter-disabled flag is global, so the module only reads it. If RTSS
has disabled the limiter globally, apply reports unavailable without changing
the flag or other applications' profiles. RTSS cannot apply the separate
`frame_limiter_disable_vsync` setting, so requests that enable it are declined
before any provider mutation. The read-only probe requires an existing regular
executable profile to inspect values; a missing profile makes a requested apply
unavailable with `rtss_profile_missing_safe_create_unsupported`. The module
does not create or delete profiles. An explicit relative RTSS install path is
accepted by settings validation but currently returns unavailable; leave the
field empty for auto-detection or supply an absolute path to the running
installation.

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

The provider-independent lease manager supports exact restore and retains a
lease after restore failure. The native RTSS adapter currently cannot execute
that write safely, so restore of a retained RTSS lease returns
`restore_failed` with `rtss_conditional_profile_write_unsupported`; its token is
kept for retry if a future provider gains safe conditional writes. A detected
external change is reported as `stale_lease` and is never overwritten. Legacy
leases from a runtime that created a profile remain stale; recovery never
deletes or overwrites that profile. The journal keeps the latest 256 completed
lease tombstones; a repeated restore returns `restored` while its tombstone
remains, then returns `not_found` after eviction. Callers should use unique
session IDs.

The lease journal is stored under the current user's `LOCALAPPDATA` (falling
back to `APPDATA`) in `Amaterasu/frame-limiter/leases.json`. The runtime API's
state-root grant is restricted to the state module. Before an RTSS mutation,
the module atomically persists a pending lease with its exact before and
intended values. On startup, a provider with safe mutation support reconciles
that record with its provider: an exact applied state becomes active, an exact
before-state is finalized as no effect, and a third state remains stale for
owner-visible recovery. If write capability is unavailable, an unverified
pending effect remains stale even when current values happen to match the
intent. Older journal records for lease-created profiles are kept stale rather
than automatically deleting or overwriting the profile. A readback mismatch is
rolled back only when the current profile and global limiter flag still match
the attempted change.

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

An explicit active `lossless-scaling` frame-generation mode also activates the
limiter policy even when no Lossless Scaling FPS limit is supplied. Its RTSS
sync type follows the generic frame-generation policy, which uses Front Edge
Sync unless virtual-display policy selects Reflex.

When the effective rate source is Lossless Scaling, the RTSS profile value is
derived from half the target rate and rounded to the nearest whole FPS, with
half-FPS ties rounded upward. This matches the reference's game-input cap for
generated output. An explicit configured frame-limiter rate keeps precedence
and is written exactly; all other rate sources also keep their exact
millihertz ratio. The policy plan continues to report the resolved LS output
target, while the RTSS lease records the derived game-input cap. Native RTSS
apply and restore remain unavailable until the provider can perform safe
conditional profile writes.

## Remaining integration and platform gates

The lifecycle contract is implemented behind the fake-provider test seam, but
Team 2 cannot receive an applied RTSS lease until RTSS supplies a safe
conditional profile-write API or a supported shared locking protocol. No Team 2
files were changed in this slice. Live RTSS process/API detection and profile
readback were not exercised because RTSS is not installed and running in the
build environment. RTSS hooks may also be blocked by a game's anti-cheat. The
community `SyncLimiter` numeric mapping and read behavior still require
verification against supported RTSS versions.
NVIDIA Control Panel, VSync control, MangoHUD, and Proton runtime providers
remain unimplemented; unsupported provider choices return `runtime_unavailable`.

Focused policy and lifecycle tests are in `src/tests.rs` and `src/lease.rs`;
strict service wire tests are inline in `src/service.rs`.
