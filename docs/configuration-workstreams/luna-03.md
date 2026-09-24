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

The coordinator later confirmed the settings-boundary commits landed on main and directed this lane to continue only pending video-owned work. Permanent-monitor creation, persistence/projection, management authority, and Windows qualification remain pending with their owners; this lane did not start the driver-protocol runtime slice. Outbox `luna-03-0016-coordinator-main-landing.json` records the landing and direction.

Next slice: correct the WGC timestamp path after tracing the existing capture controller's timestamp rewrite, then continue only with video/encode-owned work and confirmed interfaces.

## Slice 5: WGC source timestamp pacing

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `c9d8379d74e0e871695a6446a3f03e86bda75929`
- Candidate code SHA: `fd7e0ae8dd442307742655931aded1689933231b`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This slice covers `global:wgc_pacing_smoothing`, a global WGC pacing setting with a default of enabled. It corrects which monotonic timestamp anchors existing WGC phase-preserving pacing. The ID remains `pending` because the focused tests were not run and Windows capture/runtime qualification remains outstanding.

### Changed paths

- `modules/video/src/plane.rs`
- `modules/video/src/pipeline.rs`
- `modules/video/src/pipeline/pacing.rs`
- `modules/video/src/pipeline/tests.rs`

### Behavior and evidence

The initial candidate carried the available `CpuFrame.timestamp_us` alongside a local callback observation before video rewrote metadata for encoder submission. A later end-to-end trace found that `capture-wgc::Controller::stamp` had already replaced the WGC acquisition timestamp with a post-readback elapsed timestamp before it entered `CpuFrame`. Slice 5 therefore did not remove GPU readback delay from the source anchor; the follow-up v1.3 capture metadata extension is recorded in Slice 6. The source-clock mapper, fallback policy, snap window, hold limit, and encoder timestamp/sequence contract remain as described here.

Pinned Vibeshine `src/platform/windows/display_base.cpp` anchors from `capture_pacing_timestamp`, falling back to the frame timestamp and then the local steady clock. Amaterasu's WGC capture records its timestamp at frame acquisition before readback; the local observation is sampled later at the video callback. Mapping source deltas removes callback/readback delay variation while preserving a monotonic epoch usable by the current video scheduler.

### Review and validation

- Implementation worker: `/root/encode_inventory`, `gpt-6-luna` / `xhigh`; edited only the four listed video paths.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; found no code issues in the mapping/fallback paths, encoder metadata behavior, or test scope.
- Parent reviewed the source reference and final diff.
- Passed `rustfmt --edition 2024 --check modules/video/src/plane.rs modules/video/src/pipeline.rs modules/video/src/pipeline/pacing.rs modules/video/src/pipeline/tests.rs` and `git diff --check` before commit.
- Added tests cover source timestamp retention, unchanged encoder metadata, callback-delay-independent phase re-anchoring, invalid timestamp recovery, and unrepresentable deltas. Cargo tests, builds, and Clippy were not run because this lightweight worktree is reserved for source-only work.
- No Windows WGC runtime qualification was run.

### Remaining mismatches and next slice

The current source path exposes global product persistence, management editing, and launch projection in foreign-owned `modules/nvhttp` paths; those consumers were inspected read-only and still need integration-owner confirmation. No per-application or per-client scope is assigned to this setting. Slice 6 supplies the true WGC acquisition timestamp for the current v1.3 provider, but `global:wgc_pacing_smoothing` remains pending until tests run and Windows WGC runtime behavior is qualified. Older providers using v1.0-v1.2 retain the legacy post-readback timestamp fallback.

## Slice 6: WGC acquisition timestamp ABI

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `02e4c438fe6888b7fc802f9d4588c2cbd7e2d8eb`
- Candidate code SHA: `690cf08973e26fcdce69af72adb09ace2376deb8`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This slice completes the source-timestamp handoff needed by `global:wgc_pacing_smoothing` on the current WGC provider. It adds an optional additive capture API v1.3 metadata path; the ID remains `pending` until tests execute and Windows WGC behavior is qualified.

### Changed paths

- `crates/media/src/lib.rs`
- `crates/media/src/tests.rs`
- `modules/capture-wgc/README.md`
- `modules/capture-wgc/src/controller.rs`
- `modules/capture-wgc/src/model.rs`
- `modules/capture-wgc/src/native.rs`
- `modules/capture-wgc/src/readback.rs`
- `modules/capture-wgc/src/tests.rs`
- `modules/capture-wgc/src/worker.rs`
- `modules/video/src/plane.rs`

### Behavior and evidence

WGC samples its acquisition timestamp before D3D11 readback and retains that value through `PendingTexture`, `Frame`, and the controller. Controller-generated `CpuFrame.timestamp_us` and sequence remain unchanged. The v1.3 capture table appends a callback that returns the acquisition timestamp beside the unchanged `CpuFrame`; the v1.0-v1.2 structures and table prefixes keep their original layouts. Video uses the v1.3 metadata callback when the advertised minor and table size both permit it, otherwise it falls back to the v1.2 freshness callback or the v1.0 frame prefix.

Video pacing consumes the separate acquisition timestamp only for a fresh WGC frame. Cached reuse does not set a new arrival or re-anchor. Encoder submission still receives the video-generated timestamp and sequence, independent of capture pacing metadata.

### Review and validation

- Implementation worker: `/root/encode_inventory`, `gpt-6-luna` / `xhigh`; implemented the v1.3 media/capture/video path in the listed files.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; found no correctness or parity issue. It noted the lower-version negotiation test gap, which the parent addressed with a v1.0-v1.3 factory acceptance test.
- Parent added that compatibility test and updated the WGC README to document the v1.3 callback.
- Passed `rustfmt --edition 2024 --check crates/media/src/lib.rs crates/media/src/tests.rs modules/capture-wgc/src/controller.rs modules/capture-wgc/src/model.rs modules/capture-wgc/src/native.rs modules/capture-wgc/src/readback.rs modules/capture-wgc/src/tests.rs modules/capture-wgc/src/worker.rs modules/video/src/plane.rs` and `git diff --check` before commit.
- Added tests cover ABI v1.3 sizes/offsets and sink validation, WGC source timestamp retention through controller timestamp rewriting and cached reuse, older v1.2 prefix usability, v1.0-v1.3 minimum query negotiation, and video pacing/encoder metadata separation. Cargo tests, builds, and Clippy were not run because this lightweight worktree is reserved for source-only work.
- No Windows WGC runtime qualification was run.

### Remaining mismatches and next slice

Global product persistence, management editing, and launch projection for `global:wgc_pacing_smoothing` are in foreign-owned paths and need integration-owner confirmation; the ID is global-only in the assigned inventory. Providers limited to v1.0-v1.2 cannot supply a separate acquisition timestamp and retain the prior compatible fallback. Tests and Windows qualification remain outstanding. No additional self-contained video/encode setting candidate emerged without the pending capture/encoder snapshot and provider contracts; continue only when those interfaces or another video-owned runtime gap is confirmed. Keep the `global:encoder` contract request pending with Luna 02.
