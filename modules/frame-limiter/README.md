# Frame limiter policy

This crate defines typed global frame-limiter settings, inventory-scoped override
resolution, and pure provider and RTSS policy decisions. FPS settings are stored
as exact millihertz values. The legacy `rtss_disable_vsync_ullm` input is folded
into `frame_limiter_disable_vsync`; callers should persist only the canonical
key.

`ProviderPlan` is an ordered list of candidate backends and fallback conditions.
It does not start a provider, change an operating-system setting, or report that
a setting was applied. The decision model includes virtual-display and frame
generation context so a future adapter can keep the RTSS Reflex safety default
and honor explicit scoped overrides.

The Windows RTSS selector changes the configured sync type only for an active
frame-generation policy with Auto or RTSS selected. Smooth Motion uses Front
Edge Sync. Automatic virtual display selects Reflex only while its global
override is off and NVIDIA is present without an AMD adapter. Game-provided
frame generation retains Reflex on that hardware even when the global override
is on. Explicit scoped sync overrides remain authoritative. A zero FPS limit
uses, in order, a lossless scaling limit, client display-mode rate, and then an
authoritative requested-stream FPS. When that rate is absent or zero, the result
remains `FollowStream`; the future runtime adapter must supply it. Windows RTSS
relative install paths are resolved under Program Files by the future adapter.

Integration remains pending for workspace registration, product settings
versioning and persistence, management UI/edit authority, scoped override
storage, NVHTTP launch projection, OS apply/restore, and virtual-display runtime
coordination. macOS and unsupported explicit provider choices produce policy
diagnostics and no candidate backend.

Focused unit tests are included in `src/tests.rs`. This lightweight worktree is
not build-enabled, so those tests have not been run here. Integration validation
must register the crate and run `cargo test -p amaterasu-frame-limiter` in the
reserved clean source checkout.
