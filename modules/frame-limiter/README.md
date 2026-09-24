# Frame limiter service and policy

This crate defines typed global frame-limiter settings, inventory-scoped
override resolution, and provider and RTSS policy decisions. It builds as a
managed module and registers `amaterasu.frame-limiter/1`.

The version 1 `apply` request contains the immutable effective settings for one
launch plus optional stream, Lossless Scaling, explicit client display-mode,
virtual-display, and frame-generation context. Missing or null context is
unknown; the launch adapter must not infer a client display override from
stream dimensions. Rate values are exact unsigned millihertz, and zero means
the known follow-stream/no-rate value. The launch adapter resolves the six
application/client overridable fields with client-over-application-over-global
precedence. Other settings remain global. The legacy
`rtss_disable_vsync_ullm` configuration input folds into
`frame_limiter_disable_vsync`, and callers persist only the canonical field.
The RTSS install path is accepted only on Windows; empty selects auto-detection.

The service currently evaluates policy without detecting or invoking native
providers. `apply` reports `state: "policy_only"`,
`provider_availability: "unknown"`, and `applied: false`; it creates no lease.
`status` and `restore` report `state: "runtime_unavailable"` and do not claim
provider state or restoration. Unknown higher-priority rate sources make the
effective limit null. Unknown apply context makes `should_consider_apply`
null. The response exposes no provider candidates because this module does not
detect hardware or backends. Provider detection, OS apply/restore, lease
ownership, crash recovery, and platform qualification remain pending.

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

On Windows, RTSS install paths may be absolute or relative Windows paths;
relative install-path resolution belongs to the provider runtime and is not
implemented here.

Product settings versioning and persistence, management UI and edit authority,
scoped override storage, NVHTTP launch projection, app lifecycle apply/restore,
and provider runtime integration are owned by other lanes. The combined
product-settings migration is reserved at v17. Unsupported explicit provider
choices and macOS without a backend are returned as policy diagnostics; hardware
availability remains unknown and is not reported as a detected backend result.

Focused unit tests cover settings and policy in `src/tests.rs`; service wire and
validation tests are inline in `src/service.rs`. The lane worktree is not
build-enabled, so Cargo tests, builds, and Clippy have not been run here. The
coordinator should run `cargo test -p amaterasu-frame-limiter` and
`cargo clippy -p amaterasu-frame-limiter --all-targets -- -D warnings` in the
reserved clean source checkout. Windows RTSS/NVCP and Linux Proton/MangoHUD
runtime qualification remain outstanding.
