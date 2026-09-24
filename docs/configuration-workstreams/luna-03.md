# Luna 03 configuration parity handoff

## Slice 1: virtual display settings boundary

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`
- Candidate code SHA: `87b8e4143d91bb5e92901144c98f681a63ca4956`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

- `global:dd_activate_virtual_display`: global boolean, Windows-only, default `false`. It requests virtual-display activation during stream startup. It does not gate the virtual-display service's explicit temporary `Create` operation.
- `global:dd_use_sunshine_virtual_display_driver`: global boolean, Windows-only, default `true`. `false` selects the bundled SudoVDA rollback driver in Vibeshine.

Both IDs remain `pending`. This slice adds accepted module settings/defaults and observable diagnostics, but does not connect persistence, management editing, per-session settings projection, or stream-start topology activation.

### Changed paths

- `modules/virtual-display/README.md`
- `modules/virtual-display/src/manager.rs`
- `modules/virtual-display/src/manager/tests.rs`
- `modules/virtual-display/src/model.rs`

### Behavior and evidence

The service module gate (`enabled`) is separate from both configuration fields. With the module enabled, explicit temporary creation proceeds when activation is requested, while status reports stream-time activation as unsupported. Selecting SudoVDA returns `unsupported_driver` before any driver call; the service does not silently substitute the Sunshine driver. The README records these limitations.

The pinned reference documents both settings as Windows-only and gives defaults `false` for activation and `true` for the Sunshine driver (`docs/configuration.md`, headings at lines 1825 and 1847). `src/config.cpp` initializes the same defaults at lines 944-945. Vibeshine selects SudoVDA when the driver boolean is false (`src/platform/windows/virtual_display.cpp`, around line 621). The service has no SudoVDA backend, and this environment did not qualify either driver on Windows.

### Review and validation

- Implementation worker: `/root/virtual_display_inventory`, `gpt-6-luna` / `xhigh`; implemented only the four paths listed above.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; initial review found that activation incorrectly blocked explicit creation. Worker corrected it; re-review confirmed the fix and matching defaults. Reviewer also confirmed SudoVDA remains unsupported and documented.
- Parent reviewed the corrected diff.
- Passed `cargo fmt --manifest-path modules/virtual-display/Cargo.toml -- --check` and `git diff --check`.
- Added unit tests cover defaults, the separate module gate, activation diagnostics, explicit creation, and unsupported driver selection. Tests, build, and Clippy were not run because this lightweight worktree is reserved for source-only work.
- No Windows driver/platform qualification was run.

### Remaining mismatches and next slice

The SudoVDA behavior for `dd_use_sunshine_virtual_display_driver=false` is not implemented. `dd_activate_virtual_display=true` is accepted and reported but still does not activate the display or make it the only active display during stream startup. Global persistence, management edit authority, scope projection, and Windows runtime qualification remain absent. Keep both IDs pending.

Next slice: finish and review the pure configuration/policy layer under `modules/frame-limiter/**`; in parallel, coordinate launch-time projection and provider lifecycle with the owners of `modules/nvhttp/**`, `modules/application/**`, `modules/launch/**`, and management paths through the outbox. Do not close coverage until settings reach runtime and their Windows behavior is qualified.

## Slice 2: frame-limiter settings and policy

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `46bf52e127522646532d201146e8ed6098fde391`
- Candidate code SHA: `339dc21b09612249294be63fb8305e998fe578da`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This slice covers the assigned frame-limiter IDs: `global:frame_limiter_enable`, `global:frame_limiter_provider`, `global:frame_limiter_fps_limit`, `global:frame_limiter_disable_vsync`, `global:frame_limiter_auto_virtual_framegen`, `global:rtss_frame_limit_type`, `global:rtss_install_path`, `global:rtss_allow_virtual_display_override`, `global:mangohud_preset`, `global:mangohud_always_show_graph`, `global:mangohud_limiter_method`, and alias `alias:global:rtss_disable_vsync_ullm`.

The six app/client-overridable fields are enable, provider, FPS limit, disable-VSync, automatic virtual frame generation mode, and RTSS frame-limit type. RTSS install path and virtual-display override are global Windows settings. MangoHUD preset, graph, and limiter method are modeled as global settings. All IDs remain `pending`: this crate is not registered in the workspace or host, settings are not persisted or projected, management edit authority is absent, and no provider applies/restores OS state.

### Changed paths

- `modules/frame-limiter/Cargo.toml`
- `modules/frame-limiter/README.md`
- `modules/frame-limiter/src/lib.rs`
- `modules/frame-limiter/src/model.rs`
- `modules/frame-limiter/src/policy.rs`
- `modules/frame-limiter/src/tests.rs`

### Behavior and evidence

The crate models default settings and validation, exact millihertz FPS values, legacy VSync alias folding, client-over-application-over-global resolution, Windows and Linux provider plans, and RTSS frame-generation decisions. Linux Auto selects Proton with MangoHUD as an optional overlay; explicit Proton has no overlay. Windows RTSS sync changes are gated by an active frame-generation policy and Auto/RTSS selection, preserve explicit scoped values, require supported NVIDIA hardware for Reflex, and prioritize Smooth Motion's Front Edge Sync behavior. Requested stream FPS is the final effective-limit fallback after configured, Lossless Scaling, and display-mode rates when an adapter supplies that input.

Reference evidence is in Vibeshine `src/config.cpp` lines 1026, 1927-1992; `src/framegen_policy.h` lines 77-94 and 141-143; `src/platform/windows/frame_limiter.cpp` lines 160-175 and 230-298; `src/platform/linux/mangohud_policy.h` lines 42-52; and `src/platform/windows/rtss_integration.cpp` lines 1296-1315 and 1359-1390. The implementation is a new typed policy model and does not translate source code line by line.

### Review and validation

- Implementation worker: `/root/encode_inventory`, `gpt-6-luna` / `xhigh`; implemented only `modules/frame-limiter/**`.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; reviewed the initial candidate, then confirmed the corrections to Linux overlay behavior, FPS fallback, RTSS gates/overrides, RTSS path handling, parser aliases, and MangoHUD fallback diagnostics. Final re-review found no remaining code finding.
- Parent review confirmed path ownership and inspected the candidate.
- Passed `rustfmt --edition 2024 --check` for the frame-limiter Rust sources and `git diff --check` before commit.
- No Cargo build, Rust tests, Clippy, or native Windows/Linux provider qualification were run because this lightweight worktree is reserved for source-only work.

### Remaining mismatches and next slice

The frame-limiter crate is not a managed module and has no host registration; `Cargo.toml`, workspace membership, host bootstrap, and `amaterasu.frame-limiter/1` registration belong to Luna 01. Luna 02 owns product-settings v17 persistence/resolution and launch projection; Luna 05 owns lifecycle apply/restore; Luna 06 owns authorized fields and management UI. The outbox requests the exact service interface and `requested_stream_fps_millihz` runtime input. Until those interfaces arrive, the model cannot receive authoritative settings or stream context; an absent stream FPS remains `FollowStream`. RTSS/NVCP, MangoHUD/Proton hooks, virtual-display coordination, and Windows/Linux platform behavior remain unimplemented and unqualified. Keep every ID pending.

Next slice: finish the reviewed video capture-mode adapter, then implement the frame-limiter runtime integration after Luna 01/02/05/06 confirm their owned interfaces. Preserve the current v17 reservation and do not change foreign paths.

## Slice 3: video capture-mode request adapter

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `a46fa2b85e559bf1ff819cb2ab1741769817d523`
- Candidate code SHA: `484c779c286b694d6d35f84aa6748496367256b6`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This slice covers `global:capture`. It adapts canonical values `ddx`, `wgc`, `wgcc`, empty, and `auto` in a video `open` request to the existing capture-provider and constant-rate inputs. Explicit modes select their corresponding provider/rate pair. Auto and empty preserve the bootstrap provider, defaulting to DXGI only when no bootstrap provider is supplied. Legacy requests without `capture` continue to use `capture_provider` and `constant_rate`.

The ID remains `pending`: this adapter does not persist or validate the product setting, project it from launch configuration, implement provider-availability fallback, or qualify the capture implementations on Windows. The existing launch path resolves Auto against its bootstrap provider and sends `capture_provider` and `constant_rate` into the video module; the adapter keeps that contract and avoids introducing a second launch field contract.

### Changed paths

- `modules/video/src/model.rs`

### Behavior and evidence

The video request deserializer maps canonical capture strings into the typed fields already consumed by the open pipeline. `ddx` selects DXGI with constant-rate disabled; `wgc` selects WGC with constant-rate disabled; `wgcc` selects WGC with constant-rate enabled. Auto and empty use the bootstrap provider. An explicitly provided canonical mode determines constant-rate behavior even if a legacy `constant_rate` value is also present. Unknown canonical modes are rejected, and existing legacy-only payloads retain their previous default and explicit-value behavior.

The current launch contract was inspected in `modules/nvhttp/src/launch.rs` and `modules/stream-session`: launch resolution chooses an effective provider/rate pair, and stream-session passes that pair to video. The implementation changes only the video-owned request parser and does not modify foreign-owned launch code.

### Review and validation

- Implementation worker: `/root/virtual_display_inventory`, `gpt-6-luna` / `xhigh`; edited only `modules/video/src/model.rs` for this slice.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; first identified that Auto must follow the WGC bootstrap provider when supplied. After correction, final re-review found no remaining code finding.
- Parent reviewed the final diff and confirmed the provider/rate fields match the current launch-to-video contract.
- Passed `rustfmt --edition 2024 --check modules/video/src/model.rs` and `git diff --check` before commit.
- Added serde unit tests cover all canonical modes, Auto with and without a bootstrap provider, canonical-field authority over the legacy rate field, legacy compatibility, and rejection of unknown values. Tests, build, and Clippy were not run because this lightweight worktree is reserved for source-only work.
- No Windows or capture-provider platform qualification was run.

### Remaining mismatches and next slice

Product defaults, persistence, migration, management edit authority, and scoped resolution/projection for `global:capture` remain with the configuration owners. Provider availability fallback and Windows runtime qualification also remain absent. Keep `global:capture` pending.

Next slice: continue the independent assigned encoder/video settings inventory and implementation within the owned module paths. Coordinate any required settings-persistence or launch-projection contract through the outbox; do not edit another lane's paths.

## Slice 4: permanent virtual-display count boundary

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `668afd3fd0e8c451e26ac8d6ccb99efb9d359999`
- Candidate code SHA: `8cd6e30a706924a42f52273597b3b3b2488c7958`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

- `global:dd_virtual_display_permanent_count`: Windows-only, numeric default 0, accepted range 0–4; the configuration inventory allows per-application and per-client overrides.
- `alias:global:dd_vdd_static_monitor_count`: legacy global alias. It is accepted at the settings boundary, but canonical `dd_virtual_display_permanent_count` takes precedence when both names are supplied.

Both IDs remain `pending`. The virtual-display service reports a configured global value but does not implement product settings persistence, per-application/per-client resolution, management edit authority, or permanent-monitor creation.

### Changed paths

- `modules/virtual-display/src/model.rs`
- `modules/virtual-display/src/manager.rs`
- `modules/virtual-display/src/manager/tests.rs`
- `modules/virtual-display/README.md`

### Behavior and evidence

The settings boundary accepts the canonical name and legacy alias. It preserves an omitted value as unconfigured and explicit zero as configured; status reports numeric zero for an omitted value plus a separate `dd_virtual_display_permanent_count_configured` flag. If both names are supplied, the canonical name wins. Integral values outside 0–4 normalize to configured zero, following the pinned parser's default-initialize, in-range assignment behavior. Non-integer and null JSON values are rejected. The permanent count does not affect the service's existing eight-record temporary lease cap, and the service does not create permanent monitors.

Reference evidence: pinned Vibeshine `src/config.h` defines `SUNSHINE_VIRTUAL_DISPLAY_MAX_PERMANENT_COUNT` as 4. In `src/config.cpp`, the parser checks the canonical key before the legacy alias, records whether either was configured, then parses and accepts an in-range value while retaining zero for out-of-range integers. The setting inventory identifies the canonical key as Windows-only with per-application and per-client override scopes; the alias is global-only.

### Review and validation

- Implementation worker: `/root/virtual_display_inventory`, `gpt-6-luna` / `xhigh`; edited only the four paths listed above.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; confirmed canonical precedence, absent-versus-zero reporting, source range behavior, unchanged temporary limit, and the unsupported-runtime report. No code finding remained.
- Parent reviewed the final diff and the pinned parser behavior.
- Passed `rustfmt --edition 2024 --check modules/virtual-display/src/model.rs modules/virtual-display/src/manager.rs modules/virtual-display/src/manager/tests.rs` and `git diff --check` before commit.
- Added unit tests cover defaults, explicit zero, maximum 4, alias and canonical precedence, out-of-range integer fallback, non-integer rejection, and status. Tests, build, and Clippy were not run because this lightweight worktree is reserved for source-only work.
- No Windows driver/runtime qualification was run.

### Remaining mismatches and next slice

The service has no permanent-monitor creation backend. Product defaults and persistence, the reserved product-settings migration, management editing, per-application/per-client resolution and projection, and Windows qualification remain absent. Keep both IDs pending.

Next slice: resolve the pending `global:encoder` snapshot contract with Luna 02. Once the stable `EncoderIdentity` projection and provider-selection owner are confirmed, implement a bounded video-side selection adapter in the owned video/encode paths; preserve the v17 reservation and do not write foreign paths.
