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

## Slice 7: frame-limiter managed service boundary

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `5790c4ab4b38196d83cf419bc52f236e477ca165`
- Candidate code SHA: `ae2e3bb1667b57ce2dd030a6df0644db799d0479`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This slice covers the 12 frame-limiter IDs listed in Slice 2: six application/client-overridable settings, five global-only settings, and the legacy global VSync alias. The crate now exposes a strict v1 request boundary and managed-module service for the accepted name `amaterasu.frame-limiter/1`. Product persistence, scope resolution/projection, management edit authority, provider effects, and platform qualification are still missing; all 12 IDs remain `pending`.

### Changed paths

- `modules/frame-limiter/Cargo.toml`
- `modules/frame-limiter/README.md`
- `modules/frame-limiter/src/lib.rs`
- `modules/frame-limiter/src/model.rs`
- `modules/frame-limiter/src/policy.rs`
- `modules/frame-limiter/src/runtime.rs`
- `modules/frame-limiter/src/service.rs`
- `modules/frame-limiter/src/tests.rs`

### v1 request and response contract

Requests are strict objects with an `action` discriminator and `version: 1`; unknown fields and unsupported versions are rejected. `apply` requires a broker-owned non-empty `caller`, a caller-owned `session_id`, a resolved absolute `executable_path` with a file leaf, and the complete immutable effective settings object. The broker caller string is origin validation; it is not the session ID. FPS and runtime rates are unsigned exact millihertz in the model's 0..1,000,000 range. The setting values and six-field scope/precedence contract are unchanged from Slice 2.

The optional apply context is `requested_stream_fps_millihz`, `client_display_mode_override_millihz`, `lossless_scaling_limit_millihz`, `virtual_display_active`, `physical_framegen_capture`, `game_frame_generation_mode`, and `rtss_frame_limit_type_explicit_override`. Omitted or null values mean unknown. For rate fields, explicit zero is known to mean no rate/follow stream. `client_display_mode_override_millihz` is a rate-only value sourced from an explicit client display-mode override; it must never be derived from stream dimensions. The launch adapter may supply `requested_stream_fps_millihz` from the admitted `LaunchRequest.fps`. Other context without an authoritative owner remains null/omitted.

`status` accepts an optional session ID; absent/null means no session filter. `restore` requires exactly one of `session_id` or `lease`. Successful policy-only apply responses carry `state: "policy_only"`, `applied: false`, `provider_availability: "unknown"`, no lease, and no provider candidates. Unknown apply decisions and unknown higher-priority rates serialize as null; RTSS decisions remain unknown without hardware/provider facts. Status and restore currently return `state: "runtime_unavailable"`, do not report provider state or restoration, and `apply` creates no lease. Invalid requests return `{ok:false, applied:false, error:{code,message}}`.

The module exports `FrameLimiterModule` through the module SDK. `start` registers `amaterasu.frame-limiter/1`; each `tick` polls one SDK request, confirms it is active with `CheckRequest`, handles its JSON payload, and completes it. Luna 01 owns workspace/catalog registration and startup integration. Luna 02 owns the single reserved v16-to-v17 persistence migration, immutable launch settings/context projection, and scope resolution. Luna 05 owns invocation timing and teardown/restore integration. Luna 06 owns management edit authority and UI. The code candidate does not write those paths.

### Behavior and review evidence

The implementation builds a policy-only response and never claims provider application, restoration, an active lease, or detected hardware. RTSS install paths reject non-empty values off Windows; Windows accepts absolute or relative Windows paths, with relative path resolution still assigned to a future provider runtime. Frame-generation safety policy now activates on explicit physical capture or inferred GameProvided/NVIDIA Smooth Motion modes; scoped RTSS overrides retain priority, and GameProvided Reflex is not disabled by the global virtual-display override.

- Implementation worker: `/root/encode_inventory`, `gpt-6-luna` / `xhigh`; implemented only `modules/frame-limiter/**`.
- Independent reviewer: `/root/review_virtual_display`, `gpt-6-luna` / `xhigh`; re-reviewed the final candidate and confirmed prior policy, null-context, validation, and response-certainty findings were corrected. One low-severity caveat remains: the exported raw-string parser deserializes through `serde_json::Value`, so duplicate raw JSON keys become last-value-wins. The live SDK already supplies a `Value`, so duplicate keys collapsed upstream cannot be recovered by this service.
- Parent reviewed the module diff and integration boundary.
- Passed `rustfmt --edition 2024 --check` on the six Rust source files and `git diff --check` after final edits; the staged diff also passed `git diff --cached --check`.
- Service and policy unit tests were added but not run. Cargo build/tests, Clippy, and native provider qualification were not run because the lane worktree is restricted to source-only work.

### Remaining mismatches and next slice

The registered service is a request boundary, not the eventual apply/restore runtime. It does not detect or invoke RTSS, NVIDIA Control Panel, Proton, or MangoHUD, does not persist provider state or leases, and cannot restore OS state. Luna 01 still needs to register the module in the host catalog/startup paths. Luna 02 must supply the immutable effective settings and authoritative nullable context from product settings/launch. Luna 05 must define apply and idempotent restore lifecycle for spawn failure, cancellation, disconnect, stream end, and teardown. Luna 06 must provide management access under the confirmed scopes. The raw-string duplicate-key caveat is limited to `parse_operation`; the broker payload already arrives as `Value`.

Next slice: integrate only after Luna 01/02/05/06 confirm their owned interfaces. Add provider/runtime work only when OS state capture, session ownership, and restoration contracts are established. Keep all IDs pending until persistence/defaults, validation/migration, management authority, scoped launch projection, apply/restore behavior, tests, and platform qualification are reviewed together.

## Slice 8: WGC constant-rate v1.3 pacing regression

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `ccc97bcfa862085e4d6ba403f39801e4b1a1ab37`
- Worker source commit: `25d3b2215c1c9f939e0a2bcebfa30b19fb5150d9`
- Lane cherry-pick SHA: `70addda4d5ab369e83843d57b9d8ca6de4226d37`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This bounded slice covers the constant-rate WGC acquisition-timestamp and cached-reuse behavior for `global:wgc_pacing_smoothing`. The stable ID remains `pending`; this commit adds regression evidence and corrects its video-module documentation but does not supply product settings persistence, management edit authority, or launch projection.

### Changed paths

- `modules/video/src/pipeline/tests.rs`
- `modules/video/README.md`

### Behavior and evidence

The new pipeline test provides a distinct v1.3 acquisition timestamp and callback observation time, carries a pending re-anchor across cached reuse, and checks that the following fresh frame schedules from acquisition time. Existing `modules/video/src/plane.rs` tests exercise the native v1.3 callback and verify its separate pacing timestamp; this slice's pipeline regression uses `FakePlane` rather than a live native provider.

The README now documents that constant-rate WGC arrival pacing needs v1.2 freshness metadata or newer, v1.2 maps frame timestamps, v1.3 uses its separate WGC acquisition timestamp, cached reuse advances the established grid without creating a fresh arrival, and older tables retain configured read cadence without arrival-based re-anchoring. The pinned Vibeshine display loop uses `capture_pacing_timestamp` with a frame-timestamp fallback; its constant-rate cached-frame path stamps pacing time with the local steady clock. Amaterasu's WGC v1.3 callback carries the acquisition timestamp independently from callback observation.

### Review and validation

- Implementation worker: `/root/wgc_constant_rate_xhigh`, verified `gpt-6-luna` / `xhigh`; edited only the two paths listed above and committed `25d3b2215c1c9f939e0a2bcebfa30b19fb5150d9` from an isolated worktree whose parent was this lane base.
- Independent reviewer: `/root/review_wgc_v13_slice`; found no concrete findings in the timestamp assertion, cached-reuse test state, or README version/fallback claims. The reviewer context reported GPT-6 but did not expose a separate effort label.
- Parent reviewed the worker diff and cherry-picked it as `70addda4d5ab369e83843d57b9d8ca6de4226d37`.
- Passed `rustfmt --edition 2024 --check modules/video/src/pipeline/tests.rs`, `git show --check --oneline 25d3b2215c1c9f939e0a2bcebfa30b19fb5150d9`, and `git show --check --oneline 70addda4`.
- Cargo tests, build, and Clippy were not run in this lightweight worktree. Coordinator direct-to-main validation passed on `73df71733df51c63ba805e984fce406facaa207c`: format check passed, `cargo test -p amaterasu-video` passed 74/74, and strict Clippy passed.
- Windows WGC runtime qualification was not run.

### Remaining mismatches and next slice

`global:wgc_pacing_smoothing` remains pending until product defaults/persistence, management editing, global launch projection, and Windows WGC runtime behavior are reviewed. Coordinator Cargo validation is green. This setting has global scope only; no per-application or per-client override is assigned. Product-level `capture = wgcc` routing through NVHTTP, RTSP, and stream-session also remains pending as a separate capture-mode integration.

Next slice: continue a lane-owned encoder setting under `modules/encode/**` (candidate `global:nvenc_preset`), while sending any persistence, management, or launch-projection requirements to their owning lanes. Keep the frame-limiter module untouched until L01's workspace-membership gate is complete.

## Slice 9: frame-limiter policy context exhaustiveness correction

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `cb2971044c30b912fee4048aa02db08187b98b6d`
- Worker source commit: `6e0ec0753194ab2371c76c42a7e4b263bc47c5ee`
- Lane cherry-pick SHA: `31ddf95dd8ff9a71b4a80e7404616f795e21c517`
- Workspace membership main SHA: `f8fbd2fdd8222af07410ce2fcc1648618704d1d3`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This is a compile-only correction inside L03's assigned `modules/frame-limiter/**` scope. It preserves the existing `policy_context` behavior for exact millihertz context values and does not change settings, defaults, policy precedence, or stable-ID status. The service and policy candidate remain pending full product integration and qualification.

### Changed paths

- `modules/frame-limiter/src/service.rs`

### Behavior and evidence

`ApplyRequest::validate` validates requested-stream and client-display-mode millihertz values before `policy_context` conversion. Each conversion now handles `Presence::Value(0)` explicitly as `None`, then handles the exhaustive nonzero `Presence::Value(rate)` as `Some(FpsLimit)`. `Presence::Missing` and `Presence::Null` remain `None`; the validated nonzero value is no longer hidden behind a pattern guard Rust cannot treat as exhaustive.

The coordinator's priority inbox assigned this exact correction after L01's workspace-membership commit made `cargo test -p amaterasu-frame-limiter` available on main. The worker was based on the clean lane SHA above; the coordinator will cherry-pick the independent lane code commit onto main and run package tests and strict Clippy.

### Review and validation

- Implementation worker: `/root/frame_limiter_exhaustive_arms`, verified `gpt-6-luna` / `xhigh`; isolated worktree parent was `cb2971044c30b912fee4048aa02db08187b98b6d`; changed only `modules/frame-limiter/src/service.rs` and committed `6e0ec0753194ab2371c76c42a7e4b263bc47c5ee`.
- Parent reviewed the two match changes and cherry-picked them as `31ddf95dd8ff9a71b4a80e7404616f795e21c517`.
- Passed `rustfmt --edition 2024 --check modules/frame-limiter/src/service.rs` and `git diff --check` in the isolated child; parent reran rustfmt and `git show --check` on the lane commit.
- Cargo tests, build, and Clippy were not run in this lightweight worktree. The coordinator will run `cargo test -p amaterasu-frame-limiter` and `cargo clippy -p amaterasu-frame-limiter --all-targets -- -D warnings` on main.
- No native provider or platform qualification was run; this change only restores exhaustive type-level matching.

### Remaining mismatches and next slice

The module still exposes a policy-only service with no detected or applied provider, no leases or OS restoration, and `status`/`restore` remain runtime-unavailable. Product defaults/persistence, the single v16-to-v17 migration, settings projection, management edit authority, invocation/restore lifecycle, host integration, package validation, and Windows/Linux provider qualification remain pending with their assigned owners. L02 owns `modules/nvhttp/**` and the one combined v17 migration; L03 does not edit those paths.

Follow-up completed in outbox messages 0024–0027: the settings/projection contract was sent to L02, lifecycle and management requirements were sent to L05/L06, and the separately requested read-only XHigh review of L02 candidate `be11030282ae9b067c3511819c65037719161b2a` reported no findings. The later package-gate correction is recorded in Slice 10 below.

## Slice 10: frame-limiter package-gate corrections

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `609e08e64d9e646452c425056691743c6b248a23`
- Worker source commit: `780ceffc6500474f9ab6e1f1e262f97be60fc828`
- Lane code commit: `1cd6c1e6c8a6bda4d775392216cd13dab5c9e924`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### IDs and scope

This is a behavior-preserving package-gate correction in the shared `amaterasu.frame-limiter/1` apply request path, across the 12 IDs in Slice 2: `global:frame_limiter_enable`, `global:frame_limiter_provider`, `global:frame_limiter_fps_limit`, `global:frame_limiter_disable_vsync`, `global:frame_limiter_auto_virtual_framegen`, `global:rtss_frame_limit_type`, `global:rtss_install_path`, `global:rtss_allow_virtual_display_override`, `global:mangohud_preset`, `global:mangohud_always_show_graph`, `global:mangohud_limiter_method`, and `alias:global:rtss_disable_vsync_ullm`. It changes neither setting values nor runtime behavior; all IDs remain `pending`.

### Changed paths

- `modules/frame-limiter/src/service.rs`

### Coordinator gate findings and corrections

- `strict_apply_parser_preserves_null_and_absent_fields` asserted `Missing` while `virtual_display_active` and `physical_framegen_capture` were still explicit nulls in the fixture. The test retains its explicit-null assertions and removes both keys before the missing-field assertions.
- Strict Clippy requested deriving `Default` for `Presence<T>` and marking `Missing` as the default variant. The derived default remains `Missing`.
- Strict Clippy requested avoiding field reassignment after `PolicyContext::default()`. The context now uses a struct literal plus `..PolicyContext::default()`, preserving every prior explicit field mapping and default.

The zero, null, missing, and validated nonzero rate handling from Slice 9 remains unchanged. This correction does not suppress lints or alter policy semantics.

### Review and validation

- Implementation worker: `/root/frame_limiter_gate_corrections`, verified `gpt-6-luna` / `xhigh` using the session attribution script. Its isolated worktree was based on the clean lane SHA above and changed only `modules/frame-limiter/src/service.rs`; source commit `780ceffc6500474f9ab6e1f1e262f97be60fc828` uses the configured Nonary identity and required orchestration/planning provenance lines.
- Parent reviewed the worker diff and cherry-picked it into the lane as `1cd6c1e6c8a6bda4d775392216cd13dab5c9e924`.
- Worker passed `rustfmt --edition 2024 --check modules/frame-limiter/src/service.rs` and `git diff --check`; parent reran rustfmt and `git diff --check` in the lane and checked the lane commit with `git show --check`.
- Cargo format, package tests, and strict Clippy were not run in this lightweight worktree. Coordinator direct-to-main validation passed on `aa0511cb8b4cfbd57928b56ad4eec2d7b6ade763`: `cargo fmt --check -p amaterasu-frame-limiter` passed, `cargo test -p amaterasu-frame-limiter` passed 24/24, and `cargo clippy -p amaterasu-frame-limiter --all-targets -- -D warnings` passed.
- No Windows/Linux provider runtime or platform qualification was run; this correction only addresses request parsing defaults and equivalent `PolicyContext` initialization.

### Remaining mismatches and next slice

All 12 frame-limiter IDs remain pending for product persistence/defaults, the single v16-to-v17 migration, host registration, management edit authority, scoped launch projection, provider apply/restore behavior, and Windows/Linux qualification. The service remains policy-only. L02 owns the sole v17 migration and NVHTTP projection; L03 did not modify those paths.

Follow-on encoder work is recorded in Slice 11 below. Keep the frame-limiter IDs pending until product persistence/defaults and the v17 migration, host registration, management authority, scoped launch projection, provider apply/restore behavior, and Windows/Linux qualification are complete. The package gate is green on main; the policy-only runtime and its platform effects remain incomplete.

## Slice 11: NVENC preset request boundary

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `74d5afdfefa561cb6851007807d05682f7bde62a`
- Worker source commit: `c8992d8840cbc3568ff8554429f4768eaa247ce3`
- Lane code commit: `ed5f316473c0862631eb345817c9758a96d4978a`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Session: `01a0d516-85d0-7261-b883-4dddf4473f99` (`gpt-6-luna`, `max`)

### ID and scope

This bounded slice covers `global:nvenc_preset`, a global-only numeric setting. The pinned Vibeshine config documents default `1`, choices `1..=7` mapped to P1 through P7, and application only when the selected encoder is NVENC (`docs/configuration.md`, `nvenc_preset`; `src/config.cpp` range validation). The ID remains `pending`: Amaterasu's current encode module opens Media Foundation hardware transforms and has no native NVENC backend.

### Changed paths

- `modules/encode/README.md`
- `modules/encode/src/model.rs`
- `modules/encode/src/platform/windows.rs`
- `modules/encode/src/sessions/tests.rs`

### Behavior and evidence

The encode request model now represents P1 through P7 as numeric JSON values and rejects values outside that range. An omitted optional `nvenc_preset` remains absent through serialization, preserving existing request payloads. If a caller supplies this NVENC-only option to the current MFT-only backend, configuration and the direct Windows open boundary return `nvenc_preset_unsupported` before a session is opened. The service does not silently ignore the option or fall back to another encoder. No NVENC preset is applied by this slice.

The product global default remains `1` and range `1..=7`; the coordinator confirmed that L02 owns its canonical persisted field/default, the existing v16-to-v17 migration, and launch projection. The generic `modules/configuration` module is per-caller configuration, not the product-wide settings store. The product setting is global-only and must be projected only when the resolved encoder is NVENC. No extra migration or settings version is requested.

### Review and validation

- Implementation worker: `/root/nvenc_preset_slice`, verified `gpt-6-luna` / `xhigh` with the attribution script before editing. The isolated worktree was based on the clean lane SHA above and changed only the four paths listed above; source commit `c8992d8840cbc3568ff8554429f4768eaa247ce3` uses the configured Nonary identity and required provenance lines.
- Parent reviewed the worker diff, confirmed its parent and exact path set, and cherry-picked it as `ed5f316473c0862631eb345817c9758a96d4978a`. The worker and lane code commits have the same tree.
- Focused tests cover numeric P1..P7 roundtrip, rejected out-of-range forms, omitted-field compatibility, and rejection at the session-open boundary before an encoder session is created. Tests were added but not run in this lightweight worktree.
- Passed `rustfmt --edition 2024 --check` on the three Rust files and `git diff --check` in both worker and lane; the lane code commit passed `git show --check`.
- Cargo tests/build/Clippy were not run. Coordinator validation can use `cargo test -p amaterasu-encode` and `cargo clippy -p amaterasu-encode --all-targets -- -D warnings` in the clean source checkout.
- No NVENC hardware or Windows/Linux provider qualification was run. The existing ignored Windows hardware-MFT test is not evidence of NVENC preset support.

### Remaining mismatches and next slice

`global:nvenc_preset` remains pending until L02's global default/persistence and launch projection, L06's signed editor/readback, and a native NVENC backend with hardware qualification are complete. The current code rejects explicit NVENC preset requests because the only backend is MFT. Do not claim the configured preset is applied while that provider is absent.

Next action: coordinate L02's product field/projection and L06's editor against the accepted contract; continue a separate encoder/video candidate only when its behavior has an observable effect in an owned module. Keep this ID pending in the meantime.

## Slice 12: NVENC preset main validation and ownership decision

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Base SHA: `34b27f3f479b2440d15c9bb53c85b139e1626930`
- Source code commit: `ed5f316473c0862631eb345817c9758a96d4978a`
- Direct-to-main commit: `0b8d73070a245ce31e3315fa64fd362ad8945ce4`
- Stable ID: `global:nvenc_preset` (remains `pending`)

### Coordinator validation

The coordinator reports these checks passed on main at `0b8d73070a245ce31e3315fa64fd362ad8945ce4`:

- `cargo fmt --check -p amaterasu-encode`
- `cargo test -p amaterasu-encode`: 18 passed; one hardware-only MFT test ignored
- `cargo clippy -p amaterasu-encode --all-targets -- -D warnings`

These gates validate the fail-closed request boundary only. There is no native NVENC preset effect or NVENC hardware qualification.

### Accepted settings and ownership contract

The coordinator confirmed `nvenc_preset` is global-only, an integer with default `1` and range `1..=7` (P1 through P7), with no application or client override. The configured value must not be reported as applied while the native NVENC provider is absent; explicit encode requests continue to fail closed.

L02 owns the canonical product-wide field/default, the existing v16-to-v17 persistence migration, and launch projection. The generic `modules/configuration` API is per-caller configuration and is not the product-wide settings source. L06 owns the signed global editor/readback path and should present the provider-unavailable state without claiming application. No additional settings version or migration is allocated.

### Remaining parity gaps and next slice

The stable ID remains pending until L02 persistence/projection, L06 management authority/UI, a native NVENC provider effect, and hardware qualification are complete. The next independent L03 slice proposed here, automatic capture-provider fallback for canonical `capture=auto`/empty mode, is now recorded in Slice 13. It stays in L03-owned video/capture paths and does not depend on L02 persistence or L06 UI.

## Slice 13: Automatic capture-provider fallback

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Lane base SHA: `8f1923bb3ee7ad6c97d9f37e883b6cba5ecb32b7`
- Worker source commit: `c2b9971a81b113e8366724381c3a37ea8fab2b9b`
- Lane code commit: `367a15ce82e85de4749056395110cfc4ee9e28dd`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Worker session: `01a0d65b-24ad-7ea1-952f-c2c560c6e808` (`gpt-6-luna`, `xhigh`, verified before edits)
- Stable ID: `global:capture` (remains `pending`)

### Scope and behavior

Canonical empty and `auto` capture modes preserve the bootstrap-selected provider as the first choice. After a clean lower `unsupported_platform` reply or clean broker admission `unavailable`, the video pipeline may try the other provider only when it is in the configured provider allowlist. Explicit `ddx`, `wgc`, and `wgcc` modes and legacy requests that only specify `capture_provider` remain pinned. Constant-rate semantics are not changed.

The fallback enumerates only the allowed alternate provider. It derives the Windows device-name component from the source provider's output ID, then requires exactly one attached alternate output with the same exact device name and a valid alternate-provider ID shape. Missing, malformed, or ambiguous candidates retain the original provider error and are never opened. Each capture attempt has a fresh provider operation ID. The stream status preserves the requested output ID and reports the provider that actually opened it. Confirmed pre-open failures settle the parent and remove the stream without looking up resources that were never created; uncertain admission still follows the existing cancellation and settlement path.

### Changed paths

- `modules/video/README.md`
- `modules/video/src/model.rs`
- `modules/video/src/pipeline.rs`
- `modules/video/src/pipeline/lifecycle.rs`
- `modules/video/src/pipeline/tests.rs`
- `modules/video/src/rpc.rs`

### Review and validation

- Implementation worker `/root/auto_capture_fallback` used isolated worktree `D:/sources/worktrees/amaterasu-auto-capture-fallback-xhigh`, branch `codex/luna03-auto-capture-fallback-xhigh`, based on the clean lane SHA above. Its actual `gpt-6-luna` / `xhigh` session attribution was verified before edits. The worker committed only the six listed paths as `c2b9971a81b113e8366724381c3a37ea8fab2b9b` using the configured Nonary identity and required orchestration/planning trailers.
- Parent reviewed the exact source diff, verified its parent, path list, commit trailers, and clean worker state, then cherry-picked it as `367a15ce82e85de4749056395110cfc4ee9e28dd`. Worker and lane commits have the same tree (`b393a351e8c5300c1cade155b94202a57981fbdc`).
- Focused regression tests were added for both provider-ID translations, unique-output matching, operation-ID separation, clean unsupported/unavailable fallback, non-retryable errors, allowlist enforcement, uncertain admission, explicit modes, and ambiguous candidate rejection. Tests were added but not run in this lightweight worktree.
- Worker and lane passed `rustfmt --edition 2024 --check --config skip_children=true` for all five changed Rust files, `git diff --check`, and `git show --check`.
- Cargo tests/build/Clippy were not run here. Coordinator validation can use `cargo fmt --check -p amaterasu-video`, `cargo test -p amaterasu-video`, and `cargo clippy -p amaterasu-video --all-targets -- -D warnings` in the clean source checkout.
- No Windows DXGI/WGC runtime or hardware/platform qualification was performed.

### Remaining parity gaps

This module-side fallback is only one piece of `global:capture`; the ID remains pending. Product global persistence/defaults and the existing v16-to-v17 migration remain with L02. Management edit authority/readback remains with L06. Per-application/per-client projection and override behavior, host registration/integration, provider runtime qualification, and Windows DXGI/WGC effects remain open. Do not report global capture parity as complete until those product and platform paths are reviewed.

## Slice 14: Auto-capture fallback test type correction

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Lane base SHA: `c25f209f5afa14dd78bf04daeb2d05e3e3bd4932`
- Worker source commit: `cbc272ef021a3afb95ad44d814c965de52d1030f`
- Lane code commit: `138756a33adf5e6c87c25696a1a112df14553455`
- Stable ID and scope: `global:capture`; compile-only annotations for the two provider-set fixtures added in Slice 13
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`

### Coordinator finding and correction

The coordinator reported that `cargo test -p amaterasu-video` failed to compile at `pipeline/tests.rs:1201` and `:1270` on main tip `abfc8a98`. Both cloned arrays used `.into()` without a concrete target type. The correction annotates each declaration as `BTreeSet<CaptureProvider>`, matching `setup_with_providers`' parameter type. This changes no runtime behavior.

### Review and validation

- Worker `/root/capture_typefix` ran with verified `gpt-6-luna` / `xhigh` settings in isolated worktree `D:/sources/worktrees/amaterasu-auto-capture-typefix-xhigh`, based on the clean lane base above. Its session was `01a0d685-0da2-7f71-85a9-8c5e7ab340b4`.
- The worker changed only `modules/video/src/pipeline/tests.rs`, committed as `cbc272ef021a3afb95ad44d814c965de52d1030f` with the required plan trailers. Parent review confirmed the exact two annotations, parent SHA, path scope, clean worker state, and commit trailers. Cherry-pick `138756a33adf5e6c87c25696a1a112df14553455` has the same tree `51e311a2e418c28e492d2b7bbd58abc163718f8a`.
- Passed `rustfmt --edition 2024 --check --config skip_children=true modules/video/src/pipeline/tests.rs`, `git diff --check HEAD^ HEAD`, and `git show --check HEAD` in the lane.
- Cargo format, tests, build, and Clippy were not run in this lightweight lane. The coordinator will rerun video formatting, package tests, and strict Clippy after direct integration to main.
- No Windows DXGI/WGC runtime or platform qualification was run.

### Remaining mismatches and next slice

This compile correction does not close `global:capture`. Product persistence/defaults and the v16-to-v17 migration, management edit authority/readback, scoped launch projection, host integration, provider runtime behavior, and Windows qualification remain pending. Next, coordinate the frame-limiter module registration and complete settings contract with L01, including the global-only MangoHUD fields; then continue a separate owned video/display parity slice when no cross-lane contract blocks it.

## Slice 15: Frame-limiter host registration contract

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Contract evidence SHA: `ae2e3bb1667b57ce2dd030a6df0644db799d0479`; service gate correction on main: `aa0511cb`
- Current lane handoff base: `0950afb8798b28f40f7eb6614718c4748d632092`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Owner: L01 owns root workspace/host/package registration; L03 owns `modules/frame-limiter/**`.

### Complete settings contract

The service carries all 12 assigned stable IDs: `global:frame_limiter_enable`, `global:frame_limiter_provider`, `global:frame_limiter_fps_limit`, `global:frame_limiter_disable_vsync`, `global:frame_limiter_auto_virtual_framegen`, `global:rtss_frame_limit_type`, `global:rtss_install_path`, `global:rtss_allow_virtual_display_override`, `global:mangohud_preset`, `global:mangohud_always_show_graph`, `global:mangohud_limiter_method`, and alias `alias:global:rtss_disable_vsync_ullm`.

Six fields are client/application/global scoped with client-over-application-over-global precedence: `frame_limiter_enable`, `frame_limiter_provider`, `frame_limiter_fps_limit_millihz`, `frame_limiter_disable_vsync`, `frame_limiter_auto_virtual_framegen`, and `rtss_frame_limit_type`. `rtss_install_path`, `rtss_allow_virtual_display_override`, and all three MangoHUD fields are global-only. The legacy `rtss_disable_vsync_ullm` input normalizes to `frame_limiter_disable_vsync` and is never persisted as a second field.

The MangoHUD stable IDs are Linux-oriented global settings: `global:mangohud_limiter_method` defaults to `late` and accepts `early|late`; `global:mangohud_preset` defaults to `custom` and accepts `custom|1|2|3|4`; `global:mangohud_always_show_graph` defaults to `false`. None has an application or client override. These remain present in the immutable service settings object; provider effects are only relevant when a Linux MangoHUD backend is selected and available.

The v1 settings object contains all eleven canonical fields. Defaults are `frame_limiter_enable=false`, `frame_limiter_provider=auto`, `frame_limiter_fps_limit_millihz=0`, `frame_limiter_disable_vsync=false`, `frame_limiter_auto_virtual_framegen=enabled`, `rtss_frame_limit_type=async`, `rtss_install_path=""`, `rtss_allow_virtual_display_override=false`, `mangohud_preset=custom`, `mangohud_always_show_graph=false`, and `mangohud_limiter_method=late`. Rates use unsigned exact millihertz, including explicit zero. The one product-settings v16-to-v17 migration remains owned by L02; this module allocates no additional version or migration.

### Module and host interface sent to L01

- Component ID/principal: `amaterasu.frame-limiter`; Cargo package `amaterasu-frame-limiter`; library target `amaterasu_frame_limiter` (`amaterasu_frame_limiter.dll` on Windows, `libamaterasu_frame_limiter.so` on Linux, and `libamaterasu_frame_limiter.dylib` on macOS). The release packager supplies manifest file hash and size from the target artifact.
- Component manifest: use the current `amaterasu_abi::ABI_MAJOR`, `runtime=false`, empty `requires` (no service/component dependency), and `requested.provide=["amaterasu.frame-limiter/1"]`. `requested.call`, native grants, and other grants are empty for this policy-only module. The package principal for the provider must receive only the matching service registration grant.
- Consumer principal: L05's accepted lifecycle envelope says the `amaterasu.application` component invokes `apply` before process spawn. Grant that principal only `call=["amaterasu.frame-limiter/1"]`; do not grant `amaterasu.launch` or broad default callers unless L05 later changes the actual call site and updates this contract.
- Service and lifecycle: `amaterasu.frame-limiter/1`, payload version 1. `FrameLimiterModule::start` immediately registers the service and has no external-service or platform-provider readiness dependency. `tick` polls, checks request activity, handles one strict JSON payload, and completes the request. `request_ended` and `not_found` are tolerated for check/completion. A missing provider service grant causes startup registration failure.
- Requests: strict tagged `apply|status|restore` objects with `version:1`; unknown fields/versions fail. Apply requires `session_id`, resolved absolute `executable_path`, and the complete settings object above. Optional context is `requested_stream_fps_millihz`, `client_display_mode_override_millihz`, `lossless_scaling_limit_millihz`, `virtual_display_active`, `physical_framegen_capture`, `game_frame_generation_mode`, and `rtss_frame_limit_type_explicit_override`. Missing/null context means unknown; explicit rate zero remains a known no-rate value. The caller principal comes from broker metadata, not JSON. Status accepts optional `session_id`; restore requires exactly one of `session_id` or `lease`.
- Response and unsupported platforms: apply is currently policy-only (`applied=false`, `state=policy_only`, `provider_availability=unknown`, no lease or provider candidates). Status/restore return `runtime_unavailable` and claim no provider state or restoration. Unsupported provider/platform combinations are policy diagnostics, not module-start failures; the service performs no native or OS effects on any target. Validation errors use `{ok:false,applied:false,error:{code,message}}` with codes `invalid_json`, `invalid_operation`, `unsupported_version`, `invalid_identifier`, `invalid_executable_path`, `invalid_settings`, `invalid_millihertz`, and `invalid_restore_target`.
- Package profile integration: include this optional component only in explicitly selected signed profiles and preserve existing packages that omit it. Component/profile ordering follows the manifest dependency graph; no host loader branch or host ABI/schema bump is needed. The release-profile source paths `crates/release/src/profiles.rs*` are unassigned, so the coordinator must assign their owner and confirm the target package profiles before anyone edits them.

### Required integration evidence and remaining blockers

L01's package/host validation should accept the exact provider declaration and least-privilege grants, load/tick/health-check the module through the generic managed-module path, authorize `amaterasu.application` and reject ungranted callers, and preserve behavior for signed packages without the optional component. Run package/host/frame-limiter tests, strict Clippy, and signed profile validation in the reserved clean source checkout. This is a contract handoff only; no L01 paths or Cargo gates were changed here. All 12 settings IDs remain pending for product persistence/defaults, the v17 projection, management authority, scoped launch resolution, provider effects, lifecycle restore, and Windows/Linux qualification.

## Slice 16: Canonical auto-capture mode mapping correction

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Lane base SHA: `2b473921bc68132c3ec05f253323dd1697d032cf`
- Worker source commit: `562c10c614c75ec5b97d27b59e3b24418f821fbb`
- Lane code commit: `f1a1d792264fcce581d4fd7e0be7953acabc3499`
- Worker/lane tree: `068dcd159d36e3c50ee1fc783cdb832aef99c39d`
- Coordinator failure SHA: `0729e6a7300373a0131a1867dc524880581c9502`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Stable ID and scope: `global:capture`; correct canonical capture-mode flags in the video request model. The ID remains `pending`.

### Coordinator finding and correction

The coordinator reported that `cargo test -p amaterasu-video` compiled but failed 8 of 80 tests on main commit `0729e6a7300373a0131a1867dc524880581c9502`: `model::tests::canonical_auto_defaults_to_dxgi_when_no_bootstrap_provider_is_supplied`, `model::tests::canonical_capture_modes_map_to_existing_provider_and_rate_fields`, `pipeline::tests::ambiguous_candidate_output_keeps_the_original_provider_error`, `pipeline::tests::auto_retry_requires_clean_provider_rejection_and_allowlisted_candidate`, `pipeline::tests::canonical_auto_maps_wgc_device_name_to_unique_dxgi_identity`, `pipeline::tests::canonical_auto_falls_back_to_the_same_output_with_fresh_operation_ids`, `pipeline::tests::clean_unavailable_admission_retries_but_uncertain_admission_does_not`, and `pipeline::tests::explicit_capture_mode_remains_pinned_after_unsupported_provider_reply`. `CaptureMode::selection` returned the `auto_capture` and `constant_rate` booleans in the wrong combinations: `wgcc` must map to `(Wgc, false, true)`, while empty/`auto` must preserve the bootstrap provider (defaulting to DXGI), with `(provider, true, false)`. The correction changes only those production tuples; existing test expectations and other modes remain unchanged.

### Review and validation

- Worker `/root/amd_av1_latency_slice` used isolated worktree `D:/sources/worktrees/amaterasu-amd-av1-latency-xhigh`, based on the clean lane SHA above. Its actual session attribution was verified as `gpt-6-luna` / `xhigh` before edits. It changed only `modules/video/src/model.rs` and committed `562c10c614c75ec5b97d27b59e3b24418f821fbb` with the required plan trailers and configured Nonary identity.
- Parent review caught an initial inversion in the modified table expectations. The worker restored the existing intended expectations before the final commit. The reviewed final diff contains only the two production tuple changes. Parent verified the worker parent SHA, exact path list, commit trailers, attribution, and clean worker status, then cherry-picked the commit as `f1a1d792264fcce581d4fd7e0be7953acabc3499`; worker and lane trees match.
- Worker and lane passed `rustfmt --edition 2024 --check --config skip_children=true modules/video/src/model.rs`; worker and lane `git diff --check` and `git show --check` passed.
- Cargo tests, build, and strict Clippy were not run in the lightweight lane. The coordinator should cherry-pick the lane code commit directly to main and rerun `cargo fmt --check -p amaterasu-video`, `cargo test -p amaterasu-video`, and `cargo clippy -p amaterasu-video --all-targets -- -D warnings`.

### Remaining parity gaps

This correction restores the intended request-mode semantics but does not complete `global:capture`. Coordinator validation of the corrected main candidate, L02 persistence/defaults and the sole v16-to-v17 migration, L06 management authority/readback, scoped launch projection, host integration, provider runtime effects, and Windows DXGI/WGC qualification remain pending.

## Slice 18: Video pipeline strict Clippy correction

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Lane base SHA: `3a6ea040e954b355e2ff9e06889cbf47fa2f8eee`
- Worker source commit: `b58f89365fcb21f1eb302c9eac93af749f1cae4e`
- Lane code commit: `21a994e903bf0e83e8ae8092d666b6ff84bd1728`
- Worker/lane tree: `d220cf038b7216ad6e55c978d14cc29ce1fb5b66`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Coordinator source candidate: `f1a1d792264fcce581d4fd7e0be7953acabc3499`
- Coordinator main code commit: `fad027688eec7325f09fbbe565153020c624d758`
- Coordinator main handoff commit: `bccf4e08be032a0a95c92fbb0e569542e5ef1f7a`
- Scope: strict Clippy cleanup in the capture rejection path. This commit closes no stable ID; `global:capture` and all other assigned IDs remain pending.

### Correction and review

The coordinator reported `cargo fmt --check -p amaterasu-video` passed and `cargo test -p amaterasu-video` passed 80/80 on the main code candidate. Strict Clippy (`cargo clippy -p amaterasu-video --all-targets -- -D warnings`) then reported `collapsible_if` at `modules/video/src/pipeline.rs:844` and `explicit_auto_deref` at `modules/video/src/pipeline/tests.rs:60`.

The production edit combines the capture-stage, explicit rejection, and `unsupported_platform` predicates while keeping the existing fallback, failed-step, and `return false` behavior. The test edit passes the stored code through reference coercion. Parent review found no behavior changes or unrelated edits.

### Changed paths, worker, and checks

- `modules/video/src/pipeline.rs`
- `modules/video/src/pipeline/tests.rs`
- Worker `/root/video_strict_clippy_correction` used isolated worktree `D:/sources/worktrees/amaterasu-video-strict-clippy-correction-xhigh`, branch `codex/luna03-video-strict-clippy-correction-xhigh`, based on the clean lane base above. Its actual session attribution was verified as `gpt-6-luna` / `xhigh` (thread `01a0d6b7-e0f7-72e2-9fbc-923a5a5209b9`). It committed only the two paths as `b58f89365fcb21f1eb302c9eac93af749f1cae4e`, with the required plan trailers and configured Nonary identity.
- Parent verified the worker parent SHA, exact changed paths, commit trailers, model attribution, and clean worktree; reviewed the complete diff; and cherry-picked the commit as lane code commit `21a994e903bf0e83e8ae8092d666b6ff84bd1728`. The worker and lane trees match.
- Worker and lane passed `rustfmt --edition 2024 --check --config skip_children=true modules/video/src/pipeline.rs modules/video/src/pipeline/tests.rs`, `git diff --check`, and `git show --check`.
- Lane code commit `21a994e903bf0e83e8ae8092d666b6ff84bd1728` was directly cherry-picked to main as `d58139c7`. Coordinator validation on main passed formatting, all 80 video tests, and strict Clippy.

### Coordinator validation on main

- `cargo fmt --check -p amaterasu-video`: passed.
- `cargo test -p amaterasu-video`: 80 passed, 0 failed.
- `cargo clippy -p amaterasu-video --all-targets -- -D warnings`: passed.
- The source lane and branch remain available for the ongoing parity work.

### Remaining parity gaps

This is a lint-only correction and adds no persistence, defaults, validation, management authority/editor behavior, host registration, scoped launch projection, provider effect, or platform qualification. Keep `global:capture` pending until those requirements and coordinator validation are reviewed; Windows DXGI/WGC runtime qualification also remains outstanding. No stable ID is marked verified by this slice.

## Slice 19: WGC disabled-smoothing missed-slot regression

- Repository: `D:/sources/worktrees/amaterasu-config-video-display`
- Branch: `config-parity/video-display`
- Lane base SHA: `74446f3209ffd6d0280696537e2081bd0cddf1ab`
- Worker source commit: `8a01b6ec281a671cb03b53f33240864a1c77caf5`
- Lane code commit: `bc9cf356dd8c285c59838b8849d07a1472d10756`
- Worker/lane tree: `9cc463d7d0ebf3db147b6b56e4e534b1881b2e58`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Stable ID and scope: `global:wgc_pacing_smoothing`, global-only boolean, default `true`; the ID remains `pending`.
- Path reservation: outbox `luna-03-0041-wgc-disabled-smoothing-regression-reservation.json`, published before worker startup. The slice changed only `modules/video/src/pipeline/pacing.rs`.

### Regression and review

The new test, `disabled_smoothing_keeps_raw_source_anchor_after_a_missed_slot`, starts a source-clock mapping at 100,000 microseconds and monotonic time zero, records a missed slot at 16,666,666 nanoseconds, and then delivers source time 133,500 microseconds with an observation delayed to 41,500,000 nanoseconds. With smoothing disabled, it asserts the group anchor remains the raw mapped source time of 33,500,000 nanoseconds and the next slot is 50,166,666 nanoseconds. Existing enabled-mode phase snapping is unchanged. Parent review found the test setup and expected timestamps consistent with the source-clock and pacing calculations; no production behavior changed.

### Worker and checks

- Worker `/root/wgc_disabled_smoothing_regression` used isolated worktree `D:/sources/worktrees/amaterasu-wgc-disabled-smoothing-xhigh`, branch `codex/luna03-wgc-disabled-smoothing-xhigh`, based on the clean lane base above. Its verified session attribution was `gpt-6-luna` / `xhigh`, session `01a0d6d4-ae3a-7143-9b2f-d8bf5004b7e6`. It committed only `modules/video/src/pipeline/pacing.rs` as `8a01b6ec281a671cb03b53f33240864a1c77caf5`, with the required plan trailers and configured Nonary identity.
- Parent verified the exact worker parent, assigned path, commit trailers, model/effort, and clean worker worktree; reviewed the full test diff; and cherry-picked it into the lane as `bc9cf356dd8c285c59838b8849d07a1472d10756`. Worker and lane trees match.
- Worker and lane passed `rustfmt --edition 2024 --check --config skip_children=true modules/video/src/pipeline/pacing.rs`, `git diff --check`, and `git show --check`.
- Cargo tests, builds, and Clippy were not run in the lightweight worktrees. Before this test was added, the coordinator reported on main tip `36c5ab9962f63fdd2a5cd1f231ff49bde63d55b7` that formatting and strict Clippy passed and `cargo test -p amaterasu-video` passed 80/80. Those results predated this test; post-pick gates are recorded below.

### Coordinator validation on main

- Lane code commit `bc9cf356dd8c285c59838b8849d07a1472d10756` landed as `bb44a712`; handoff commit `05de5a156c04c2dc6b2542323d86fd690fde0130` landed as `2ec332b3`.
- `cargo fmt --check -p amaterasu-video` passed; `cargo test -p amaterasu-video` passed 81/81, including the new disabled-smoothing regression; strict Clippy passed.
- WGC runtime qualification and the persistence, launch projection, and management editor dependencies remain outstanding.

### Remaining parity gaps

This is regression coverage only; it does not add or change WGC pacing behavior. Keep `global:wgc_pacing_smoothing` pending until the new test executes in the coordinator checkout, Windows WGC runtime behavior is qualified, and L02 persistence/defaults plus L05 global launch projection and L06 management readback/editor behavior are complete. No stable ID is marked verified by this slice.

## Coordinator validation update: NVENC H.264 CAVLC test compile fix

- L03's reviewed lane candidate `42bd52e8978f65247d2ec81456cce895a5f7626a` landed directly as `90b1b320c347b660b763c2707d2deafc5ff722b4`. It replaces the test's `assert_eq!` with a boolean equality assertion, keeping the production `Configuration` type unchanged.
- Main validation passed `cargo fmt --check -p amaterasu-encode`, `git diff --check`, 21 encode tests (one hardware test ignored), and strict Clippy.
- `global:nvenc_h264_cavlc` remains pending settings/defaults and scoped projection, management readback/editor behavior, the actual provider effect, and Windows hardware qualification.

## Coordinator validation update: WGC smoothing retry identity (2026-09-25)

- L03's reviewed test commit `afb9300feab74cf4237cb278cf6f8caca930b4ad` landed directly on main as `89463e1c`. Coordinator validation found that the second open response was still asynchronous when the test read `runtime.completed`; the test was reading the prior conflict response.
- Coordinator commit `6135151` retries the second operation with the same operation ID and settings before inspecting the response. This verifies the returned stream 2 snapshot is `wgc_pacing_smoothing: true` while stream 1 remains false, without allocating another stream.
- `cargo fmt --check -p amaterasu-video`, `git diff --check`, all 82 video tests, and `cargo clippy -p amaterasu-video --all-targets -- -D warnings` passed on main after the correction.
- This slice is regression coverage only. `global:wgc_pacing_smoothing` remains pending management readback and Windows WGC qualification; current main already persists the global v17 value, and no application/client override is part of this contract. No stable ID is closed by this update.
