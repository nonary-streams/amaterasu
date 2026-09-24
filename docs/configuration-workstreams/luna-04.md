# Luna-04: audio and input configuration parity

## Current bounded slice

- Candidate code commit: `50163b7dff4eefc4e4de14638d2861940c1f849e`
- Lane start/base commit: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Pinned behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- Branch: `config-parity/audio-input`
- Session: `01a0d516-8713-7b83-87f1-d35c0828ae10`
- Main model and reasoning: `gpt-6-luna`, `max` (verified from the active thread attribution database)
- Lane paths changed: `modules/input/**` only

This slice adds a Windows ViGEm DS4 report path for an explicit `gamepad: "ds4"` selection and wires `ds4_back_as_touchpad_click` through input policy version 9 and optional lease acquisition. It advances these IDs locally, but neither ID is complete or verified:

| Stable ID | Local work in this slice | Remaining parity work |
| --- | --- | --- |
| `global:gamepad` | Windows explicit DS4 target allocation/report update is supported when both ViGEmClient DS4 exports are present. Standard buttons, d-pad, triggers, sticks, PS, touchpad click, cleanup and effective profile reporting are wired. Existing X360 behavior remains on its own path. | Auto still selects X360 only and has no VHF fallback; VHF/UMDF, DS5, Switch and other reference profiles remain unimplemented. The existing management editor and per-application/per-client launch routing need coordinator lane integrations. Linux reference profiles and runtime qualification remain incomplete. Installed-driver target/device behavior is unqualified. |
| `global:ds4_back_as_touchpad_click` | The global bool defaults to true, ProductSettings v8 migrates to v9 with true, management-facing input updates accept the value, and `amaterasu.input/1` accepts a sparse per-acquire bool. Enabled Back remains Share and also presses DS4 touchpad; disabled Back remains Share. Touchpad and miscellaneous-button bits still press DS4 touchpad. | Luna06 management/schema and Luna02 launch/stream-session routing must apply sparse client > application > global resolution. Luna05 owns the application catalog side. Native DS4 runtime behavior is unqualified. |

The DS4 report uses the pinned Vibeshine mapping for stick conversion, including its X=127/Y=129 encoding for a zero-valued snapshot. An independent reviewer compared this with ViGEm's initial 0x80 center and reported the one-unit offset. The mapping is retained to match the pinned reference implementation; its exact expectations are covered by authored mapping tests.

The report currently carries buttons, d-pad, triggers and sticks. It does not implement DS4 touch coordinates, motion, rumble, or controller feedback. DS4 availability checks the ViGEmClient DS4 allocation and update exports at lease start; no ViGEmBus device was available in this lane for runtime qualification.

## Changed paths

- `modules/input/README.md`
- `modules/input/src/backend/mod.rs`
- `modules/input/src/backend/windows.rs`
- `modules/input/src/backend/windows_gamepad.rs`
- `modules/input/src/controller.rs`
- `modules/input/src/controller/tests.rs`
- `modules/input/src/gamepad.rs`
- `modules/input/src/gamepad_profile.rs`
- `modules/input/src/lib.rs`
- `modules/input/src/model.rs`
- `modules/input/src/product_settings.rs`
- `modules/input/src/product_settings/tests.rs`

## Validation and platform evidence

- `rustfmt --edition 2024 --check` on changed Rust files: passed.
- `git diff --check`: passed.
- Rust tests, builds and Clippy: not run; this lightweight worktree explicitly prohibits them.
- Windows ViGEm target/device qualification: not run; no installed bus/device environment was available.
- Authored but not run: DS4 button/d-pad/trigger/stick mapping, report layout, DS4 Back/touchpad behavior, v8-to-v9 defaulting, explicit false parsing, operation replay conflict, and scoped-over-global setting restoration.
- Independent review: no other source findings. Reviewer could not verify its runtime model/reasoning settings from the available task context.

Run these checks from the coordinator's clean source checkout:

```powershell
cargo fmt --all -- --check
cargo test -p amaterasu-input
cargo test -p amaterasu-input --target x86_64-pc-windows-msvc
cargo clippy -p amaterasu-input --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

The Windows test target covers pure DS4 mapping and policy tests, not installed ViGEmBus delivery. A separate Windows machine with the supported ViGEmClient/ViGEmBus pair must qualify target creation, updates, visibility and removal.

## Dependencies and workers

The coordinator confirmed the contract in `coordinator-luna-04-ds4-input-decision-01`: global default true, sparse boolean scope overrides, client > application > global resolution, DS4-only effect, unchanged X360, ProductSettings v9, unchanged `amaterasu.input/1`, and no binary ABI change. Luna02 owns NVHTTP and stream-session consumers; Luna06 owns route/UI changes; Luna05 owns app catalog integration. Follow-up contract details were sent in outbox `002-dependency-gamepad-scope.json`. No foreign-owned files were edited.

Two implementation workers were requested as `gpt-6-luna`/`xhigh` on separate read-only reconnaissance tasks (`modules/input` and `modules/audio`). Neither could verify its live model/effort or expose a distinct session ID, so neither received edit authorization. The first candidate was inspected by a separate read-only reviewer, who also could not verify its settings. All code in commit `50163b7dff4eefc4e4de14638d2861940c1f849e` was authored and reviewed by the lane lead. Startup verification is recorded in `outbox/luna-04/000-session-verification.json`; the v9 acknowledgement and review record are in `outbox/luna-04/003-coordinator-ack-v9-review.json`.

## Remaining assigned IDs

The coordinator owns the coverage ledger. Leave every assigned ID pending until full behavior, persistence/defaults, validation/migration, management edit authority, scoped override behavior, tests and applicable platform qualification are reviewed. In particular, this slice does not close any ID. The rest of the 23-ID audio/input assignment remains in existing implementation or pending status and still needs per-ID parity review.

The lane-start coverage snapshot reported all 23 IDs pending. This candidate does not change the shared ledger:

| Stable ID | Scope | Lane status |
| --- | --- | --- |
| `global:always_send_scancodes` | global | pending |
| `global:audio_sink` | global | pending |
| `global:audio_sink_capture_only` | global | pending |
| `global:back_button_timeout` | global | pending |
| `global:controller` | global | pending |
| `global:ds4_back_as_touchpad_click` | global, per-application, per-client | pending; local slice only |
| `global:ds5_inputtino_randomize_mac` | global | pending |
| `global:gamepad` | global, per-application, per-client | pending; local slice only |
| `global:high_resolution_scrolling` | global | pending |
| `global:install_steam_audio_drivers` | global | pending |
| `global:key_repeat_delay` | global | pending |
| `global:key_repeat_frequency` | global | pending |
| `global:key_rightalt_to_key_win` | global | pending |
| `global:keybindings` | global | pending |
| `global:keyboard` | global | pending |
| `global:motion_as_ds4` | global | pending |
| `global:mouse` | global | pending |
| `global:native_pen_touch` | global | pending |
| `global:proton_dualsense_compatibility` | global | pending |
| `global:remote_monitor_mute_audio` | global | pending |
| `global:stream_audio` | global | pending |
| `global:touchpad_as_ds4` | global | pending |
| `global:virtual_sink` | global | pending |

## Next bounded slice

Continue with Windows audio capture recovery in `modules/audio/src/worker.rs` and its owned platform/packet files. The audit found that `device_lost` currently ends capture permanently, while the pinned reference retries acquisition until shutdown. Keep the AudioCapture API stable, make retry shutdown-cancellable, preserve packet sequence/timestamp continuity and mark the first recovered packet discontinuous. Add fake-source retry/continuity tests but do not execute them in this lightweight checkout. Then continue independent audio and input settings work while cross-lane consumers integrate.
