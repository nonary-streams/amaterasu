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