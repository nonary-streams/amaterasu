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

Windows `apply` supports RTSS when the request selects `rtss` or `auto`, policy
requires a limiter, the limit is known, RTSS is running, and its profile API is
available. The module finds the running `RTSS.exe`, loads the sibling hook DLL
from that installation, and applies the exact rational FPS limit,
denominator, and sync-limiter type to the executable profile. It reads all
three values back before returning `applied: true` and a lease. `auto` has no
unimplemented fallback: if RTSS cannot be used, the response is
`runtime_unavailable`. Other providers and non-Windows RTSS requests also
remain unavailable.

The RTSS limiter-disabled flag is global, so the module only reads it. If RTSS
has disabled the limiter globally, apply reports unavailable without changing
the flag or other applications' profiles. RTSS cannot apply the separate
`frame_limiter_disable_vsync` setting, so requests that enable it are declined
before any provider mutation. Settings that cannot be read and restored exactly
are also declined. An explicit relative RTSS install path is accepted by
settings validation but currently returns unavailable; leave the field empty
for auto-detection or supply an absolute path to the running installation.

A successful lease belongs to the broker caller plus `session_id`. Repeating
apply with the same owner, session, executable, and frozen request returns the
same lease while RTSS still matches. Changed settings or target for that owner
and session return `lease_conflict`; the runtime also allows only one active
RTSS lease at a time. `status` lists only that caller's leases and may filter by
session. `restore` accepts exactly one `session_id` or lease token, compares
RTSS with the lease-owned values before restoring, then verifies the original
profile values or deletes a profile created by the lease. A changed provider
state is reported as stale and is never overwritten. Restored lease tombstones
make repeated cleanup return `restored`; foreign or unknown tokens return
`not_found`.

The lease journal is stored under the current user's `LOCALAPPDATA` (falling
back to `APPDATA`) in `Amaterasu/frame-limiter/leases.json`. The runtime API's
state-root grant is restricted to the state module. Before an RTSS mutation,
the module atomically persists a pending lease with its exact before and
intended values. On startup it reconciles that record with RTSS: an exact
applied state becomes active, an exact before-state is finalized as no effect,
and a third state remains stale for owner-visible recovery. A readback mismatch
is rolled back only when the current values still match the attempted change.

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
build environment. RTSS hooks may also be blocked by a game's anti-cheat. NVIDIA
Control Panel, VSync control, MangoHUD, and Proton runtime providers remain
unimplemented; unsupported provider choices return `runtime_unavailable`.

Focused policy and lifecycle tests are in `src/tests.rs` and `src/lease.rs`;
strict service wire tests are inline in `src/service.rs`.
