# Luna 05 applications handoff

## Startup verification

- Session: `01a0d516-8830-7960-af1e-079a13415f97` (CLI process PID 20804; lane wrapper PID 23228).
- Repository root: `D:/sources/worktrees/amaterasu-config-applications`.
- Branch and starting HEAD: `config-parity/applications` at `30acd5de34ac5548ebf4d46086fc32ccad7b6913`.
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Active CLI invocation specifies `gpt-6-luna`, `model_reasoning_effort="max"`, three child threads, `gpt-6-luna` default subagent model, and `xhigh` default subagent effort. Runtime process arguments match. The current-session model settings are verified from the actual process invocation; server-side model attestation is not separately exposed by the available session tools.
- Inherited file access: `danger-full-access`, with the lane worktree and run directory provided to this process. Task ownership remains restricted to the assigned module trees and this handoff file; no writes outside those paths are planned.
- Initial worktree status was clean.
- Execution plan, inventory, coverage ledger, and coordinator ledger were read. Reference commit: `457237db7e57f9c4be21c35bb07e28d1c100eb7d`.
- Initial coverage state: all 77 assigned IDs are `pending` in the coordinator-owned ledger.

## Slice 0: startup and inventory

- Base/candidate SHA: `30acd5de34ac5548ebf4d46086fc32ccad7b6913` / same (documentation not committed yet).
- IDs/scopes: all 77 assigned global, per-application, and alias IDs; no status changes.
- Paths: this handoff only. No module code changed.
- Workers: `steam_slice` and `launchers_slice` were spawned with explicit `gpt-6-luna/xhigh` settings. The worker environment does not expose a separate model/effort introspection API; dispatch arguments are the available settings evidence. `steam_slice` implemented Slice 1 in its assigned files. `launchers_slice` independently reviewed that candidate, then performed a read-only Playnite path-setting audit; findings are pending.
- Tests: none run. Rust builds/tests/Clippy are prohibited in this lightweight worktree; coordinator must validate the integrated candidate in the reserved clean source checkout.
- Platform evidence: checkout inspected on Windows. No native or platform qualification run.
- Remaining: inspect all application IDs and consumers, request any foundation/management contracts, implement focused slices, format/review/commit, and submit candidate evidence. No IDs are verified.
- Next slice: investigate raw-command preparation parity and the catalog's broad sparse override contract while continuing independent settings work.

## Slice 1: Steam exclusion matching

- Base SHA: 30acd5de34ac5548ebf4d46086fc32ccad7b6913.
- Code candidate SHA: 0d54977cd625173f73abe6be7e29a96d07a9f8b5.
- Stable ID/scope: global:steam_exclude_games (global Steam catalog-selection policy). Status remains pending; this change covers only paired ID/name matching.
- Exact code paths: modules/steam/src/settings.rs, modules/steam/src/catalog_sync.rs, modules/steam/README.md.
- Behavior: an exclusion record matches if its canonical decimal ID matches the Steam app ID or if its name matches the current game name using ASCII case-insensitive comparison; when both are supplied either can exclude. The reference uses a byte-oriented lowercase helper, so Unicode case mappings such as Kelvin sign to ASCII k are not applied.
- Worker: steam_slice, requested as gpt-6-luna/xhigh; implemented settings matching, unit coverage and README wording. Formatting command cargo fmt --package amaterasu-steam passed; git diff --check passed.
- Independent reviewer: launchers_slice, requested as gpt-6-luna/xhigh; re-reviewed final candidate with no blocking findings. Reviewer confirmed paired matching, validation edges and the Kelvin-sign regression assertion.
- Tests run: none. Rust tests/build/Clippy remain prohibited in this lightweight worktree. Integration validation command: cargo test -p amaterasu-steam from the reserved clean source checkout.
- Platform evidence: comparison policy is platform-independent ASCII matching, but no runtime platform qualification was run.
- Remaining Steam gaps: global product settings and runtime/persistence/UI still require end-to-end review; root discovery, durable/converted artwork, stale-entry behavior and launch/process qualification remain as documented in modules/steam/README.md. The Steam README gap list now distinguishes the implemented app-type/play-history selection readers from remaining discovery and artwork gaps.
- Next slice: continue auditing a bounded application setting with an observable runtime gap; keep all Steam IDs pending until full persistence, editor, platform and regression evidence is reviewed.

## Slice 2: preserve sparse catalog overrides in launch records

- Base/candidate SHA: c30bf0db56f7c7a306b11957e143d8461e4b734a / e77ee240b57cb111d77c518cf9359f256e77a9e2.
- Stable ID/scope: per_application:config-overrides; launch response and status snapshot propagation only. Coverage remains pending until management editing, full consumer precedence and platform evidence are reviewed.
- Exact paths: modules/launch/src/protocol.rs, modules/launch/src/pipeline/tests.rs, modules/launch/README.md.
- Behavior: launch responses now retain all ten optional catalog overrides: fec_percentage, stream_audio, audio_sink, virtual_sink, audio_sink_capture_only, back_button_timeout, controller, gamepad, keyboard and mouse. Catalog revisions are snapshotted into launch records, so later catalog edits do not change an existing response. Explicit false values and empty endpoint selectors remain present. Virtual sink and gamepad values use the catalog bounds and profile validation.
- Worker: launchers_slice, requested as gpt-6-luna/xhigh; implemented the three exact paths.
- Lead review: checked catalog validation, launch/status record retention, and NVHTTP consumption. Independent reviewer steam_slice reviewed commit e77ee240b57cb111d77c518cf9359f256e77a9e2 read-only and reported no findings; the reviewer confirmed explicit false/empty preservation, catalog-aligned bounds, and downstream precedence handling.
- Formatting/checks: worker ran rustfmt --edition 2024 and git diff --check successfully.
- Tests run: no Rust tests/build/Clippy in this lightweight worktree. Integration command: cargo test -p amaterasu-launch from the reserved clean source checkout.
- Platform evidence: selector shape is serialized without platform-specific conversion. No native runtime qualification was run.
- Remaining: all ten override fields already exist in catalog and NVHTTP. Management surface currently exposes the existing ten fields. Coordinator's next sparse stream override request adds max_bitrate, minimum_fps_target and video_max_batch_size_kb at app scope; these will be handled in Slice 3. No IDs are verified.
- Next slice: add additive optional per-application sparse video overrides and preserve their values through launch; then request management-surface support from lane 06.

## Slice 3: sparse per-application video override storage and launch snapshot

- Base/candidate code SHA: `89c9a5da7804381f634e0d0fa2ce2b5c9c37c36f` / `a9cffb1b4acdf9fb0cf5e00face2abc3ff144b37`.
- Stable IDs/scopes: `global:max_bitrate`, `global:minimum_fps_target`, and `global:video_max_batch_size_kb`, per-application scope only. All remain pending.
- Exact paths: `modules/catalog/src/model.rs`, `modules/catalog/src/pipeline.rs`, `modules/catalog/src/pipeline/tests.rs`, `modules/catalog/README.md`, `modules/launch/src/protocol.rs`, `modules/launch/src/pipeline/tests.rs`, and `modules/launch/README.md`.
- Behavior: catalog and launch retain optional `max_bitrate: i32`, `minimum_fps_target: f64`, and `video_max_batch_size_kb: u8`. Explicit zero remains distinct from absent; FPS must be finite in 0..=1000 and batch size in 0..=64. Provider marker version 1 remains compatible because new fields default to `None`. Launch and status keep the values captured at the catalog revision used to start the application. Effective values and `<=0` bitrate behavior still belong to NVHTTP.
- Worker: `stream_scopes`, requested as `gpt-6-luna/xhigh`; changed only the authorized catalog model/pipeline/tests and launch protocol/tests paths. Added positive bitrate and marker persistence round-trip coverage after lead review.
- Independent reviewer: `steam_slice`, requested as `gpt-6-luna/xhigh`, found P1: NVHTTP's strict app override parser rejects any populated new field as `invalid_application_response`, and the RTSP payload still resolves bitrate, FPS, and batch size from globals only. The finding is external lane 02 work, not resolved in this candidate.
- Cross-lane request: outbox `011-nvhttp-stream-consumer-dependency.json` supplies the exact lane 02 paths, types, migration question, precedence and acceptance contract. Outbox `012-stream-lutris-review-findings.json` records the reviewer evidence. Do not integrate this partial candidate before lane 02 adds strict parsing, validation, client-bound persistence, precedence resolution and per-stream snapshots.
- Formatting/checks: worker ran `cargo fmt --package amaterasu-catalog --package amaterasu-launch` and `git diff --check`; lead ran `cargo fmt --check --package amaterasu-catalog --package amaterasu-launch --package amaterasu-lutris --package amaterasu-playnite` and `git diff --check` successfully.
- Tests: no Rust tests, builds or Clippy in the lightweight worktree. Integration commands: `cargo test -p amaterasu-catalog`, `cargo test -p amaterasu-launch`, and `cargo test -p amaterasu-nvhttp` from the reserved clean source checkout.
- Platform evidence: JSON persistence is platform-neutral; no runtime platform qualification. Lane 02 must prove stream behavior on qualified platforms.
- Remaining: per-client management editor and identity-bound persistence are lane 06/02 dependencies; consumer resolution is lane 02. Keep all three IDs pending. Candidate is committed but blocked for integration by the strict consumer parser and missing precedence wiring.
- Next slice: fix an independent provider selection edge while waiting for lane 02's consumer implementation.

## Slice 4: paired Lutris exclusion alternatives and absent-row cleanup

- Base/candidate SHA: `a9cffb1b4acdf9fb0cf5e00face2abc3ff144b37` / `abb31538a22327712b4c5d9c23e8908bfcac93ba`.
- Stable ID/scope: `global:lutris_exclude_games`, global Lutris catalog-selection policy. Status remains pending.
- Exact paths: `modules/lutris/src/settings.rs`, `modules/lutris/src/catalog_sync.rs`, and `modules/lutris/README.md`.
- Behavior: paired metadata excludes a game by matching either decimal ID or ASCII-case-insensitive name. A paired name also triggers revision-bound inventory, allowing a previously imported, now-absent Lutris row to be removed by its last attached name when stale removal is disabled. ID cleanup remains active; current rows, detached rows and other providers/users are protected.
- Worker: `launchers_slice`, requested as `gpt-6-luna/xhigh`, implemented paired matching and regression coverage, then completed a follow-up after confirming the same settings to include paired names in inventory cleanup. Its exact follow-up write authorization was limited to `catalog_sync.rs` and the Lutris README.
- Independent reviewer: `steam_slice`, requested as `gpt-6-luna/xhigh`, re-reviewed the final change and found no blocking findings. The review checked inventory revision/CAS, fixed-provider scoping, detached rows, current-row matching, and ID-only exclusions.
- Formatting/checks: worker ran `cargo fmt --package amaterasu-lutris` and `git diff --check`; lead ran the workspace format check and diff check successfully.
- Tests: no Rust tests/build/Clippy in the lightweight worktree. Integration command: `cargo test -p amaterasu-lutris` from the reserved clean source checkout.
- Platform evidence: comparison and inventory selection logic is independent of the Linux Lutris database adapter; no native Lutris/runtime qualification was run.
- Remaining: global settings persistence/editor defaults, scan/platform behavior and provider runtime parity still require full review. The exclusion ID remains pending.
- Next slice: continue with remaining independent application settings and wait for lane 02/06 consumer responses.

## Playnite documentation alignment note

- Code base/candidate SHA: `abb31538a22327712b4c5d9c23e8908bfcac93ba` / same; documentation is updated in the following handoff commit.
- Related IDs: `global:playnite_fullscreen_entry_enabled` and `per_application:playnite-fullscreen`; both remain pending coordinator review of full runtime, editor, and platform evidence.
- Exact path: `modules/playnite/README.md`.
- Finding and update: the README incorrectly said the fullscreen setting and action were still pending because the catalog could not represent the host action, despite the implemented persisted setting and typed `playnite_fullscreen` launch target. The README now describes the typed target as the safe application representation and the global toggle as generated-row control. No runtime behavior changed.
- Tests/platform evidence: documentation-only change; no tests or platform qualification.
- Next slice: audit remaining Playnite behavior and management coverage; do not infer verified parity from the typed launch target alone.

## Steam README reconciliation note

- Code base/candidate SHA: `0d54977cd625173f73abe6be7e29a96d07a9f8b5` / same; documentation is updated in the following handoff commit.
- Related ID: `global:steam_exclude_games`, still pending full settings/editor and provider parity review.
- Exact path: `modules/steam/README.md`.
- Finding and update: the gap list called app-type/play-history selection pending while the same README documented those selection readers. It now identifies those readers as implemented when local sidecars are available and leaves root discovery/artwork/launch gaps explicit. No runtime behavior changed.
- Tests/platform evidence: documentation-only change; no tests or platform qualification.
- Next slice: keep the Steam ID pending until full behavior evidence is reviewed.

## Slice 5: raw preparation commands and Playnite working directories

- Base SHA: `e9491cb03e008a10a1152bd018e2ffbecce50b3f`.
- Code candidate SHA: `13f09110530d9d76ffacc004af6559d1acde2c76`; later candidate head after provider fixes: `1e310501c20a11ff3579527a39d243c8ee6d5b2a`.
- Stable IDs/scopes: `global:global_prep_cmd`, `per_application:prep-cmd`, `per_application:elevated`, `per_application:exclude-global-prep-cmd`, `per_application:working-dir`, `per_application:config-overrides`, and `per_application:dd-configuration-option`; all remain pending. Related assigned global policy IDs `global:max_bitrate`, `global:minimum_fps_target`, and `global:video_max_batch_size_kb` also remain pending; this slice stores only app-scoped override values.
- Exact paths: `modules/application/README.md`, `modules/application/src/lifecycle.rs`, `modules/application/src/processes.rs`, `modules/application/src/protocol.rs`, `modules/application/src/tests.rs`, `modules/catalog/README.md`, `modules/catalog/src/model.rs`, `modules/catalog/src/pipeline.rs`, `modules/catalog/src/pipeline/tests.rs`, `modules/catalog/src/protocol/tests.rs`, `modules/launch/README.md`, `modules/launch/src/pipeline.rs`, `modules/launch/src/pipeline/tests.rs`, `modules/launch/src/protocol.rs`, `modules/launch/src/records.rs`, and `modules/launch/src/workflow.rs`.
- Behavior: catalog retains exact top-level raw `prep_cmd`/`prep-cmd` entries and typed Playnite working directories. Launch snapshots structured and raw global/application steps, applies global exclusion to both global forms, preserves the configured directory separately from an inferred main-executable directory, and forwards environment/cwd to the application preparation lifecycle. Raw steps are tokenized without invoking a shell, expand only `$(NAME)` and `$$` at execution, preserve exact command strings for identity, and share bounded cancellation/undo reporting. Playnite structured app steps inherit the configured directory unless a step supplies its own. Working directories are capped at 1024 bytes to match Launch/Application validation.
- Worker assignments: `stream_scopes` (`gpt-6-luna/xhigh`) implemented raw application lifecycle and app context; `launchers_slice` (`gpt-6-luna/xhigh`) implemented catalog/launch raw snapshots and Playnite working-directory propagation. Both confirmed the requested settings before editing and ran formatting/diff checks only.
- Independent review: `steam_slice` (`gpt-6-luna/xhigh`) reviewed the final catalog/launch candidate. It found and then re-reviewed fixes for Playnite structured app-step cwd inheritance and catalog/downstream working-directory bound mismatch. No remaining blocker was reported for those fixes.
- Cross-lane blocker: NVHTTP has a strict `deny_unknown_fields` application-override parser and rejects newly populated video/frame-limiter fields in launch records. Lane 02 must update that consumer and resolve precedence/client snapshots before the sparse override slice can be integrated. This is reported in outbox `022-stream-overrides-consumer-blocker.json`.
- Tests/checks: Rust tests, builds and Clippy were not run. Lead ran `cargo fmt --check --package amaterasu-application --package amaterasu-catalog --package amaterasu-launch --package amaterasu-lutris --package amaterasu-playnite --package amaterasu-steam` and `git diff --check`; both passed. Integration commands: `cargo test -p amaterasu-application -p amaterasu-catalog -p amaterasu-launch` in the reserved clean source checkout.
- Platform evidence: only the Windows checkout was inspected; no OS runtime qualification was run. Raw preparation differs from the pinned source in documented ways: it has bounded timeouts and null stdio, stops undo at the first failure, does not implement the permission-denied exception, and does not resolve Windows URL/file associations. These are recorded in `modules/application/README.md`.
- Restricted-worktree incident: a worker inadvertently attempted `cargo check -p amaterasu-application` despite the lightweight-worktree restriction. Its report did not include an available result, and no result is relied on. The coordinator should run the clean integration commands before accepting this candidate.
- Remaining: NVHTTP consumer/precedence and client snapshots (lane 02), app management editing (lane 06), frame-limiter service/package grants (lanes 03/01), source-specific raw command edge cases, and platform qualification. No IDs are verified.
- Next slice: continue independent provider settings fixes while leaving downstream-dependent overrides pending.

## Slice 6: bounded Steam legacy settings inputs

- Base/candidate SHA: `13f09110530d9d76ffacc004af6559d1acde2c76` / `294d6adc3e225e92193cf902e391a826097f0ec2`.
- Stable IDs/scopes: `global:steam_exclude_games`, `global:steam_recent_games`, and `global:steam_recent_max_age_days`; all remain pending.
- Exact paths: `modules/steam/src/settings.rs` and `modules/steam/src/tests.rs`.
- Behavior: typed exclusion arrays accept legacy string IDs and metadata objects; stored legacy JSON-array strings and comma-separated strings are parsed within 256-entry/256-KiB bounds. Recent limits accept signed 32-bit semantics, mapping negatives to defaults and rejecting values above `i32::MAX`. Serialization stays in canonical metadata-object form. Name matching and duplicate detection use ASCII case folding.
- Worker/review: `stream_scopes` (`gpt-6-luna/xhigh`) implemented the parsing tests. Lead reviewed the case-folding semantics and added Kelvin-sign-versus-ASCII-k coverage; this candidate has not had a post-fix independent full Steam-module review.
- Tests/checks: no Rust tests/build/Clippy. The owned-package format check and `git diff --check` passed. Integration command: `cargo test -p amaterasu-steam` in the reserved clean checkout.
- Remaining: management updates accept string IDs in arrays but not one whole CSV string; strict validation still rejects malformed source IDs. Runtime discovery, UI and platform/provider behavior are outside this parser slice. No Steam IDs are verified.
- Next slice: review provider discovery and management behavior separately; retain these IDs as pending.

## Slice 7: preserve Playnite rows selected by multiple sources

- Base/candidate SHA: `294d6adc3e225e92193cf902e391a826097f0ec2` / `1e310501c20a11ff3579527a39d243c8ee6d5b2a`.
- Stable ID/scope: `global:playnite_sync_all_installed`; remains pending.
- Exact path: `modules/playnite/src/catalog_sync.rs`.
- Behavior: selection cleanup now removes only rows with `source_flags == SOURCE_INSTALLED`. Rows combining installed and recent flags remain when recent-only selection is active and no replacement exists. Added a regression test for this combined-source row.
- Worker/review: `launchers_slice` (`gpt-6-luna/xhigh`) implemented the condition and regression. Independent `steam_slice` review confirmed it matches the pinned Playnite `installed` source semantics.
- Tests/checks: no Rust tests/build/Clippy. The all-owned-packages format check and `git diff --check` passed. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean checkout.
- Platform evidence: no Playnite runtime qualification; only source-selection transaction logic is covered.
- Remaining: settings persistence/editor/default parity and Windows runtime/provider qualification. No ID is verified.
- Next slice: continue Playnite runtime and management review.

## Slice 8: make Lutris removal assertion order-independent

- Base/candidate SHA: `bdf7a54921025c5a551598ac217cd0cbb29b932e` / `e9491cb03e008a10a1152bd018e2ffbecce50b3f`.
- Stable ID/scope: `global:lutris_exclude_games`; remains pending.
- Exact path: `modules/lutris/src/catalog_sync.rs`.
- Behavior: the test now asserts the exact paired-exclusion removal ID set without requiring an order between base and inventory removals. Production transaction behavior is unchanged.
- Worker: isolated child `lutris_order_child` (`gpt-6-luna/xhigh`) committed `777d2c291c9d8cb8d3a8ad33bcaca24ea6fefc72`; root reviewed the single-file diff and cherry-picked it as `e9491cb03e008a10a1152bd018e2ffbecce50b3f`. Both commit messages contain the required plan attribution lines.
- Tests/checks: no Rust tests/build/Clippy. The child did not run tests; lead's all-owned-packages `cargo fmt --check` and `git diff --check` passed. Integration command: `cargo test -p amaterasu-lutris` in the reserved clean checkout.
- Remaining: this only removes an accidental ordering assertion; global settings/editor and provider runtime parity remain incomplete. No ID is verified.
- Next slice: retain the Lutris ID as pending until complete provider evidence is reviewed.

## Slice 9: align Lutris exclusion uniqueness with matching

- Base/candidate SHA: `2b6697c3705910c75be6960f1d5b2db8f98e84ef` / `efbe98a58ecba92d5bd13aa1348948809447fe15`.
- Stable ID/scope: `global:lutris_exclude_games`, global Lutris catalog selection; remains pending.
- Exact paths: `modules/lutris/src/settings.rs` and `modules/lutris/README.md`.
- Behavior: uniqueness validation now uses the same ASCII case-folding rule as the runtime matcher and includes each supplied ID and name alternative, so paired records cannot overlap through either key. Unicode case-folding equivalences are not introduced. Repeated exclusions remain more strictly rejected than by the pinned parser.
- Worker: `lutris_settings_parity`, requested as `gpt-6-luna/xhigh` and confirmed before editing; changed only the two listed paths and added duplicate-key regression cases.
- Independent reviewer: `steam_slice` reviewed the committed candidate read-only and found no concerns; it compared the ASCII matcher and stricter duplicate behavior against the pinned Lutris policy.
- Checks: `cargo fmt --package amaterasu-lutris` and `git diff --check` passed. Rust tests/build/Clippy were not run in this lightweight worktree. Integration command: `cargo test -p amaterasu-lutris` in the reserved clean source checkout.
- Platform evidence: byte-oriented matching is platform-independent; no Lutris runtime or Linux database qualification was run.
- Remaining: duplicate exclusions differ from source acceptance; settings persistence/editor, provider runtime, and platform behavior still need full review. No ID is verified.
- Next slice: continue independent provider settings work while keeping `global:lutris_exclude_games` pending.

## Slice 10: align Playnite recent setting integer semantics

- Base/candidate SHA: `efbe98a58ecba92d5bd13aa1348948809447fe15` / `479470ab1a72502ddda4f8757cb90f9bc76116cc` (`fix(playnite): align recent settings integer semantics`).
- Stable IDs/scopes: `global:playnite_recent_games` and `global:playnite_recent_max_age_days`, global Playnite settings. Both remain pending.
- Exact paths: `modules/playnite/src/settings.rs`, `modules/playnite/src/lib.rs`, `modules/playnite/src/tests.rs`, and `modules/playnite/README.md`.
- Behavior: both settings use signed 32-bit storage semantics; arbitrary Amaterasu upper bounds were removed. Negative recent-game counts are preserved and disable recent selection. Negative maximum age values are clamped to zero on persisted-document load and management update; validation rejects a negative stored age. Out-of-range persisted numbers fall back to host defaults 10 and 30, while out-of-range management updates are rejected.
- Worker: `playnite_recent_settings`, requested and confirmed as `gpt-6-luna/xhigh`; implemented the four listed paths. Lead fixed the reviewed negative-age persistence gap before commit.
- Independent reviewer: `playnite_age_fix_reviewer` was dispatched read-only as `gpt-6-luna/xhigh` and found no issue in the four-file candidate. The review tool could not independently attest the actual model or reasoning effort.
- Checks: the all-owned-package `cargo fmt --check --package amaterasu-application --package amaterasu-catalog --package amaterasu-launch --package amaterasu-lutris --package amaterasu-playnite --package amaterasu-steam` and `git diff --check` passed. Rust tests/build/Clippy were not run. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean source checkout.
- Platform evidence: source defaults and signed integer parsing were compared with pinned Playnite configuration/parser and runtime code; no Playnite Windows runtime qualification was run. Reference management UI normally constrains values to 0..50 games and 0..3650 days; web-schema age default is 0 while host config default is 30. Host numeric-prefix strings such as `12tail` remain unsupported in Amaterasu's typed JSON settings, and settings retain the host default of 30.
- Remaining: raw numeric-prefix compatibility, editor range/default parity, and Windows provider/runtime qualification remain. Both IDs stay pending until persistence, management editing, selection behavior, and platform evidence are reviewed end to end.
- Next slice: audit `global:playnite_exclude_categories` end to end against the pinned parser and selection policy, then implement any independent provider-side mismatch.

## Slice 11: align Playnite excluded-category matching

- Base/candidate SHA: `799d43b0a2fced8cd33fb09016015745425aba2f` / `b4c4049333015daaedbb0c8a1d13cd641ebd8fe4`.
- Stable ID/scope: `global:playnite_exclude_categories`, global Playnite selection policy; remains pending.
- Exact paths: `modules/playnite/src/snapshot.rs` and `modules/playnite/README.md`.
- Behavior: excluded category names now use ASCII lowercase comparison to match the pinned byte-oriented `std::tolower` path under the default C locale. Included categories, plugin names, and game IDs retain their existing Unicode lowercase behavior. A Kelvin-sign category remains selected when excluding ASCII `k`.
- Worker: `playnite_categories_slice`, dispatched as `gpt-6-luna/xhigh`; changed only the two listed paths and added an inline matching regression. The worker could not independently introspect runtime model/effort; dispatch settings were explicit.
- Independent reviewer: `provider_settings_reviewer`, dispatched as `gpt-6-luna/xhigh`, found no issue in this candidate. The reviewer could not introspect runtime model/effort.
- Checks: worker ran `rustfmt --edition 2024 modules/playnite/src/snapshot.rs` and `git diff --check` for the changed Playnite files; lead ran the all-owned-package `cargo fmt --check` and `git diff --check`. Rust tests/build/Clippy were not run. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean source checkout.
- Platform evidence: matching uses Rust's ASCII-only case conversion, independent of platform; no Windows Playnite runtime qualification was run. Reference behavior uses locale-sensitive `std::tolower`; non-default C locale behavior is unqualified.
- Remaining: editor/persistence coverage and full Windows provider/runtime qualification remain. Keep the ID pending for coordinator's full end-to-end review.
- Next slice: audit the Playnite auto-sync timer and refresh lifecycle against the pinned runtime policy in `global:playnite_auto_sync`.

## Slice 12: fall back on persisted Steam recent-setting overflow

- Base/candidate SHA: `b4c4049333015daaedbb0c8a1d13cd641ebd8fe4` / `739b07944d23c95a87d0aa3f6f61c9f6e465acfa`.
- Stable IDs/scopes: `global:steam_recent_games` and `global:steam_recent_max_age_days`, global Steam selection settings; both remain pending.
- Exact paths: `modules/steam/src/settings.rs` and `modules/steam/README.md`.
- Behavior: persisted negative and out-of-range values now fall back per field to defaults 10/30 instead of invalidating the whole product document. The nonnegative i32 range through 2,147,483,647 remains accepted. Integral overflow represented by serde_json as floating point also falls back, while fractional floats remain rejected. The README's prior 10,000/36,500 limits were replaced with the actual signed 32-bit bound and zero behavior.
- Worker: `steam_recent_slice`, dispatched as `gpt-6-luna/xhigh`; changed only the listed files and added regression coverage for each field, u64 overflow, an integer token beyond u64, and fractional rejection. The worker could not independently introspect runtime model/effort; dispatch settings were explicit.
- Independent reviewer: `provider_settings_reviewer`, dispatched as `gpt-6-luna/xhigh`, initially found P2 for oversized JSON integer tokens reaching `visit_f64`; lead added integral overflow fallback and a raw-token regression, and the reviewer re-reviewed the correction with no remaining findings. Reviewer could not introspect runtime model/effort.
- Checks: worker ran `cargo fmt --package amaterasu-steam` and `git diff --check`; lead reran the all-owned-package `cargo fmt --check` and `git diff --check`. Rust tests/build/Clippy were not run. Integration command: `cargo test -p amaterasu-steam` in the reserved clean source checkout.
- Platform evidence: integer parsing is platform-independent; no Steam runtime or OS qualification was run.
- Remaining: product settings still require JSON integers, so Vibeshine's `std::stoi` numeric-prefix forms (for example `15tail`) are not supported. Management updates use typed unsigned fields and reject values above i32::MAX, while source editor inputs clamp to nonnegative values. Persistence, editor, sidecar-selection runtime, and platform parity need full review; both IDs stay pending.
- Next slice: audit the `global:steam_enabled` lifecycle from management update through discovery and refresh, keeping platform qualification explicit.

## Slice 13: correct Lutris enabled-state documentation

- Base/candidate SHA: `62d700eadd7e67c65ea85f56c6317aedbde107c0` / `07376468d5c86d9463809b865b4657c6814a8792`.
- Stable ID/scope: `global:lutris_enabled`, provider enable/disable lifecycle; remains pending.
- Exact path: `modules/lutris/README.md`.
- Behavior evidence: the Linux-on default matches Vibeshine; disabling blocks new discovery/reconciliation but leaves already imported rows in both implementations. The README no longer calls retained rows a parity gap. It now records the in-flight difference: Amaterasu drops discovery snapshots tied to the old settings epoch, while the pinned Vibeshine worker may reconcile one scan using settings captured before disable. Runtime code was not changed.
- Worker: `lutris_enabled_slice`, dispatched as `gpt-6-luna/xhigh` for a read-only policy audit followed by this documentation correction. The worker could not independently introspect runtime model/effort; dispatch settings were explicit.
- Lead review: inspected the README diff against the reported provider/runtime paths; no independent reviewer was used for this documentation-only correction.
- Checks: `git diff --check` passed. No tests/build/Clippy were run. No runtime/platform qualification was performed; the checkout is Windows and Linux behavior was compared only to the pinned source.
- Remaining: in-flight snapshot cancellation differs from Vibeshine and remains an intentional stronger behavior unless the coordinator requests exact transition parity. Full settings persistence/editor and Linux Lutris database/runtime qualification are still required. Keep the ID pending.
- Next slice: finish the active `global:playnite_auto_sync` audit, then take the next independent provider lifecycle slice.

## Slice 14: preserve Playnite membership during manual refresh

- Base/candidate SHA: `4b030895d2fd0b45fdc3c1196a0a1e650a4bc647` / `f45bd43d677d489f41c7a74c98cd258741c4e669` (`fix(playnite): preserve manual refresh membership`).
- Stable ID/scope: `global:playnite_auto_sync`; manual refresh behavior while auto-sync is disabled. The ID remains pending.
- Exact paths: `modules/playnite/src/catalog_sync.rs` and `modules/playnite/README.md`.
- Behavior: when `auto_sync` is false, a complete snapshot updates metadata only for already attached Playnite rows with matching prepared game IDs. It preserves each row's existing source flags and does not add, remove, replace, or change membership. After a successful refresh, launch eligibility is rebuilt only from those matching attached rows; this is required because a settings update clears the provider's eligibility set and launch requests reject IDs outside it.
- Worker: `playnite_autosync_slice`, dispatched as `gpt-6-luna/xhigh`, implemented the refresh filtering and regression test in the two exact paths.
- Review and correction: `provider_settings_reviewer` found a P1 in the first candidate: auto-sync disable clears launch eligibility, so suppressing publication would reject all subsequent Playnite launches. The lead changed eligibility publication to the attached-row/snapshot intersection and kept publication conditional on successful transaction finish. The reviewer re-reviewed the corrected candidate and found no remaining blocker. The pinned reconciler may update any existing app matched by Playnite ID; Amaterasu's stricter update scope is limited to attached provider-owned rows.
- Checks: lead ran `cargo fmt --check --package amaterasu-playnite` and `git diff --check`; both passed. No Rust tests/build/Clippy were run. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean source checkout.
- Platform evidence: source policy and launch gating were compared in the Windows checkout; no Playnite Windows runtime qualification was run.
- Remaining: Amaterasu polls every 30 seconds and retries while stopped or after failure, while Vibeshine starts on configuration apply and reconciles extension snapshots with a three-second debounce. The timer/source-trigger difference, narrower attached-row edit authority, settings editor/persistence, and Windows runtime qualification remain open. Keep `global:playnite_auto_sync` pending.
- Next slice: audit Lutris auto-sync fingerprinting and scan scheduling; keep the Lutris IDs pending until runtime and Linux qualification are complete.

## Slice 15: align included Playnite category matching

- Base/code-candidate SHA: `f45bd43d677d489f41c7a74c98cd258741c4e669` / `1c67cab966c1fa1e3763474a28e19126a74be416` (`fix(playnite): match included categories with ASCII rules`).
- Stable ID/scope: `global:playnite_sync_categories`; remains pending.
- Exact paths: `modules/playnite/src/snapshot.rs` and `modules/playnite/README.md`.
- Behavior: included category names now use ASCII lowercase matching, matching the pinned byte-oriented `std::tolower` behavior just as excluded categories do. A configured ASCII `k` does not match a Playnite category named `K`; ASCII case-insensitive matches continue to work. The README now documents the rule for both include and exclude lists.
- Worker: `playnite_sync_categories_slice`, dispatched as `gpt-6-luna/xhigh`, changed `snapshot.rs` and added the ASCII/Kelvin-sign regression.
- Independent reviewer: `provider_settings_reviewer`, dispatched as `gpt-6-luna/xhigh`, reviewed the candidate and reported no findings. The reviewer could not independently introspect model or reasoning effort.
- Checks: lead ran `cargo fmt --check --package amaterasu-playnite` and `git diff --check`; both passed. No Rust tests/build/Clippy were run. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean source checkout.
- Platform evidence: byte-oriented matching is platform-independent, but no Windows Playnite runtime or non-default C locale qualification was run.
- Remaining: management editing/persistence and Playnite Windows runtime qualification remain. Keep `global:playnite_sync_categories` pending.
- Next slice: compare Lutris auto-sync scanning against source fingerprint and interval behavior; document unimplemented fingerprint/artwork inputs if they cannot be represented in Amaterasu's current snapshot.

## Slice 16: align Playnite provider filters with bytewise matching

- Base/code-candidate SHA: `bc086004e88d723286272cae9de2f3b1e276755b` / `ae1a0aa75a9286ee1327acfc7a9a3fc96d14605e` (`fix(playnite): match provider filters bytewise`). The Playnite patch is commit `ae1a0aa75a9286ee1327acfc7a9a3fc96d14605e`; its parent also contains the separately committed Lutris slice.
- Stable IDs/scopes: `global:playnite_sync_categories`, `global:playnite_exclude_categories`, `global:playnite_sync_plugins`, `global:playnite_exclude_plugins`, and `global:playnite_exclude_games`; all remain pending.
- Exact paths: `modules/playnite/src/settings.rs`, `modules/playnite/src/snapshot.rs`, and `modules/playnite/README.md`.
- Behavior: category, plugin ID, and game ID filters now use ASCII lowercase comparisons throughout inclusion, exclusion, and snapshot ID de-duplication. Filter uniqueness validation uses the same ASCII normalization, so Unicode case equivalents such as `k` and `K` remain distinct while repeated ASCII values are still rejected. Regression cases cover category inclusion, plugin inclusion/exclusion in both Kelvin-sign directions, game ID exclusion in both directions, parser preservation of distinct IDs, and settings validation.
- Worker: `playnite_plugin_ascii_audit`, dispatched as `gpt-6-luna/xhigh`, implemented plugin matching and the initial tests/README update in the authorized Playnite paths. The worker could not independently introspect model or effort; dispatch settings were explicit. The lead extended the candidate to category/game filters, snapshot ID de-duplication, and validation after review identified Unicode validation folding.
- Independent review: `provider_settings_reviewer`, dispatched as `gpt-6-luna/xhigh`, found no remaining code parity findings after the validation correction. The reviewer confirmed the pinned Windows process sets `LC_ALL` to `C`; the README's obsolete non-default-locale caveat was corrected. The reviewer could not independently attest model or reasoning effort.
- Checks: lead ran `cargo fmt --check --package amaterasu-playnite --package amaterasu-lutris` and `git diff --check`; both passed. Rust tests, builds, and Clippy were not run in the lightweight worktree. Integration command: `cargo test -p amaterasu-playnite` from the reserved clean source checkout.
- Platform evidence: matching policy was compared to the pinned byte-oriented parser/selection code and its fixed C locale. No Playnite Windows runtime qualification was run.
- Remaining: Amaterasu still rejects repeated ASCII filter values although the pinned parser does not enforce that uniqueness rule; management editor/persistence and Windows runtime qualification also need end-to-end review. Keep all five IDs pending.
- Next slice: audit one remaining Steam or Playnite lifecycle transition against the pinned runtime, including defaults, persisted settings, and management update behavior.

## Slice 17: reconcile Lutris catalog snapshots only when source inputs change

- Base/code-candidate SHA: `bc086004e88d723286272cae9de2f3b1e276755b` / `1eadfa6a3b03932fd8a56ca17839ff69bbf03836` (`fix(lutris): detect catalog-relevant snapshot changes`).
- Stable ID/scope: `global:lutris_auto_sync`, automatic Lutris catalog reconciliation policy; remains pending.
- Exact paths: `modules/lutris/src/discovery.rs`, `modules/lutris/src/lib.rs`, `modules/lutris/src/catalog_sync.rs`, and `modules/lutris/README.md`.
- Behavior: automatic scans skip catalog reconciliation when the ordered local source signature is unchanged, except for a five-minute retry. The signature includes each game's ID, name, runner, service, service ID, directory, and config path, plus the resolved Lutris executable because those inputs affect selection or catalog output. Settings changes invalidate the cached signature, explicit manual refresh bypasses the signature gate, and the signature is retained only after a validated catalog Finish. The next automatic attempt is scheduled 30 seconds after discovery, optional Steam metadata read, and catalog work complete. Lutris artwork paths/revisions and artwork/icon file metadata remain absent from the local snapshot, so the retry cannot refresh artwork.
- Worker: `lutris_auto_sync_audit`, dispatched as `gpt-6-luna/xhigh`, implemented the signature gate, retry scheduling, catalog Finish receipt, and focused regression coverage in the four authorized Lutris paths. The worker could not independently introspect model or effort; dispatch settings were explicit.
- Independent review: `provider_settings_reviewer` initially found that service classification and the resolved executable were omitted even though they affect Steam selection and `launch.program`. The lead added both to the signature with regression assertions and updated the README. The reviewer re-reviewed the corrected candidate and reported no remaining findings. The pinned source fingerprint also omits service, while Amaterasu includes it to avoid stale local selection output. Reviewer model/effort attestation was unavailable.
- Checks: worker ran `rustfmt --check --edition 2024` on the Lutris code paths and `git diff --check`; lead ran `cargo fmt --check --package amaterasu-playnite --package amaterasu-lutris` and `git diff --check`; all passed. Rust tests, builds, and Clippy were not run. Integration command: `cargo test -p amaterasu-lutris` from the reserved clean source checkout.
- Platform evidence: scan policy was compared with the pinned Lutris fingerprint and scheduling code; the candidate worktree is Windows, and no Linux Lutris database/runtime qualification was run.
- Remaining: artwork discovery/refresh, durable settings/editor coverage, native Lutris runtime qualification, and end-to-end management persistence remain. Keep `global:lutris_auto_sync` pending.
- Next slice: audit remaining provider settings and lifecycle IDs; preserve the artwork fingerprint limitation and platform gaps as open requirements.

## Slice 18: reconcile Steam catalogs only when source inputs change

- Base/code-candidate SHA: `d29028e306ba057aaa134959ed9dcc92eaa81a04` / `47604651b267ee00a197bb34e81e926a6e5665de` (`fix(steam): avoid syncing unchanged snapshots`).
- Stable ID/scope: `global:steam_auto_sync`, automatic Steam discovery and catalog reconciliation; remains pending coordinator review of full parity.
- Exact paths: `modules/steam/src/catalog_sync.rs`, `modules/steam/src/tests.rs`, and `modules/steam/README.md`.
- Behavior: Steam discovery now schedules the next automatic scan 30 seconds after discovery and any catalog reconciliation attempt finish, and will not start a periodic scan during an active catalog transaction. A deterministic fingerprint covers every current game field represented by the provider, snapshot completeness flags, selected artwork revision/path/remote state, and the Steam executable path. An unchanged complete snapshot skips reconciliation until the five-minute artwork retry deadline; settings changes invalidate the cache. Only a validated Finish receipt caches the fingerprint; failed/incomplete attempts remain uncached and retry on the next 30-second cycle. A newer provider revision with identical fingerprint does not cancel an active transaction.
- Worker: `steam_auto_sync_audit`, dispatched as `gpt-6-luna/xhigh`, implemented fingerprinting, retry/scheduling behavior, focused unit and fake-host regressions, and README parity notes in the three listed Steam paths. Dispatch settings were explicit; the worker could not independently attest its effective model or effort.
- Independent review: `provider_settings_reviewer` and a nested read-only reviewer inspected the candidate and reported no remaining material issue. Lead also reviewed the diff and reran formatting/diff checks. Reviewers could not independently attest effective model or reasoning effort.
- Checks: worker ran `cargo fmt --package amaterasu-steam`, `cargo fmt --check --package amaterasu-steam`, and `git diff --check`; all passed. Lead reran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; both passed. Rust tests, builds, and Clippy were not run in the lightweight worktree. Integration commands: `cargo test -p amaterasu-steam` and `cargo clippy -p amaterasu-steam --all-targets -- -D warnings` in the reserved clean source checkout.
- Platform evidence: compared discovery/catalog scheduling with the pinned Steam behavior from the Windows checkout; no native Steam runtime or cross-platform qualification was run. Amaterasu's provider does not represent all Vibeshine fingerprint inputs: artwork candidate paths and file metadata, session artwork revisions, and playtime cannot independently trigger reconciliation. The `refresh` action refreshes discovery but does not force reconciliation with `auto_sync=false`, unlike Vibeshine's authenticated force-sync endpoint. Settings persistence/editor parity also remains open.
- Remaining: full management persistence/editor review, unrepresented source/artwork inputs and force-sync behavior, and Steam runtime/platform qualification. Keep `global:steam_auto_sync` pending until the coordinator reviews end-to-end evidence.
- Next slice: audit a remaining assigned provider setting or lifecycle ID independently, preserving the explicit Steam fingerprint and manual-refresh gaps above.

## Slice 19: make Steam enabled-policy regression portable

- Base/code-candidate SHA: `284836c1ad0731485773d8c633f1b997153bf051` / `be049899051f7ca61b65332bae6b9c1c3f790c1e` (`test(steam): respect Linux enabled policy`).
- Stable ID/scope: `global:steam_enabled`, provider enable/disable policy; remains pending full persistence, management, and platform review.
- Exact path: `modules/steam/src/tests.rs`.
- Behavior/evidence: the pinned Steam configuration forces `enabled=true` on Linux, including persisted and update values; Amaterasu's `ProductSettings::for_host` applies the same normalization. The settings rollback test incorrectly asserted disabled provider state on all hosts. Its assertion now expects the host-specific Linux policy. This is a test portability fix; runtime behavior did not change. The module README already documents the Linux exception accurately.
- Worker: `steam_enabled_slice`, dispatched and confirmed as `gpt-6-luna/xhigh`, audited the pinned configuration and current settings/update/discovery path, then changed only the authorized test assertion.
- Independent review: `provider_settings_reviewer` confirmed the assertion against the pinned Linux policy and current host normalization; no findings. The reviewer could not independently attest effective model or effort.
- Checks: lead ran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; both passed. A direct `rustfmt --check modules/steam/src/tests.rs` reported unrelated existing formatting differences, while the package-level format check passed. Rust tests, builds, and Clippy were not run. Integration command: `cargo test -p amaterasu-steam` in the reserved clean source checkout.
- Platform evidence: Linux policy was compared against pinned parser/configuration behavior; no native Linux Steam runtime qualification was run. Windows and other non-Linux runtime behavior still need qualification.
- Remaining: full settings persistence/editor and lifecycle review, plus native platform qualification. Keep `global:steam_enabled` pending.
- Next slice: complete the disjoint Playnite plugin-filter audit for `global:playnite_sync_plugins` and `global:playnite_exclude_plugins`, including persisted duplicate values and management-update validation.

## Slice 20: accept duplicate Playnite plugin filters

- Base/code-candidate SHA: `ef27fc897584f3ef46af8be0a938bd2200f22d8b` / `d78096431d2293b0658ded8551d7d483638b0b7b` (`fix(playnite): accept duplicate plugin filters`).
- Stable IDs/scopes: `global:playnite_sync_plugins` and `global:playnite_exclude_plugins`, global Playnite plugin selection filters; both remain pending.
- Exact paths: `modules/playnite/src/settings.rs`, `modules/playnite/src/tests.rs`, and `modules/playnite/README.md`.
- Behavior: pinned Vibeshine parsing preserves duplicate plugin IDs and runtime membership folds them into sets, making repeats inert. Amaterasu now permits repeated IDs in only `sync_plugins` and `exclude_plugins`; it retains bounds and value validation, and continues requiring uniqueness for category and game filters. Plugin defaults remain empty, persisted values round-trip, and omitted management updates remain optional. Validation error text no longer incorrectly says plugin IDs must be unique.
- Worker: `playnite_plugin_slice`, confirmed from its live session record as `gpt-6-luna/xhigh`, audited the pinned parser/runtime and implemented plugin-only duplicate acceptance plus settings, management decode, and documentation coverage. Snapshot selection already handled duplicates as sets, so `snapshot.rs` needed no change.
- Independent review: `provider_settings_reviewer` found a P3 stale validation error claiming plugin filters must be unique. The lead corrected the text and added an exact-message regression; the reviewer re-reviewed with no remaining findings. Reviewer model/effort attestation was unavailable.
- Checks: worker ran `rustfmt --edition 2024 --check modules/playnite/src/settings.rs modules/playnite/src/tests.rs` and `git diff --check`; lead ran `cargo fmt --check --package amaterasu-playnite` and `git diff --check`; all passed. Rust tests, builds, and Clippy were not run. Integration command: `cargo test -p amaterasu-playnite` in the reserved clean source checkout.
- Platform evidence: policy was compared with the pinned parser and runtime set matching on the Windows host; no native Playnite runtime qualification was run.
- Remaining: Amaterasu stores plugin IDs as strings while Vibeshine can preserve `{id,name}` metadata for display. End-to-end editor representation/persistence and Windows runtime qualification remain. Keep both IDs pending.
- Next slice: audit another independent assigned provider or launch setting, retaining the Playnite metadata and runtime gaps for coordinator review.

## Slice 21: match Steam runtime-name filtering bytewise

- Base/code-candidate SHA: `68b6417c7e72a2302851c786a5f2d6e504dc96a5` / `c7672084b25c22308cc899f9781a2523047cda42` (`fix(steam): match bytewise runtime filtering`).
- Stable ID/scope: `global:steam_include_tools`, Steam app-type and runtime-name filtering when tools are excluded; remains pending full settings/editor and platform review.
- Exact paths: `modules/steam/src/catalog_sync.rs`, `modules/steam/README.md` (the regression test is inline in `catalog_sync.rs`).
- Behavior: pinned Vibeshine lowercases UTF-8 name bytes with `std::tolower` under `LC_ALL=C`. Amaterasu used Unicode lowercase for runtime-name fallbacks, so `SteamworKs Common Redistributables` could be transformed into the excluded ASCII runtime name. Amaterasu now lowercases ASCII bytes only; ASCII app-type filtering and the `include_tools=true` override were already aligned. A regression checks exact ASCII filtering, Kelvin-sign identity, and uppercase ASCII app-type filtering.
- Worker: `steam_include_tools_slice`, confirmed from live session state as `gpt-6-luna/xhigh`, implemented the code, test, and README correction in the authorized Steam files.
- Independent review: `provider_settings_reviewer` found no findings; it confirmed bytewise classification matches the pinned C-locale policy and the Kelvin-sign regression distinguishes Unicode folding. Reviewer model/effort attestation was unavailable.
- Checks: worker ran `cargo fmt --check --manifest-path modules/steam/Cargo.toml` and `git diff --check`; lead ran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; all passed. Rust tests, builds, and Clippy were not run. Integration command: `cargo test -p amaterasu-steam` in the reserved clean source checkout.
- Platform evidence: behavior was compared to pinned Steam classification and C-locale initialization from the Windows source checkout. No native Steam runtime or cross-platform qualification was run.
- Remaining: `global:steam_include_tools` still needs complete management/editor persistence review and provider/runtime qualification. Keep it pending. The separate P3 documentation correction from the exact `global:steam_auto_sync` review remains queued as its own XHigh worker slice.
- Next slice: correct the Steam auto-sync README wording in a separate committed slice, then resume an independent provider setting while the Lutris export-authorization request is pending.

## Slice 22: correct Steam auto-sync revision documentation

- Base/candidate SHA: `8c4bddb1980a4a29e0021be66be56bb7f3f071c3` / `9256d862f938f1183e92f71504b7731b3120fa1c` (`docs(steam): clarify snapshot revision retries`).
- Stable ID/scope: `global:steam_auto_sync`, documentation correction only; remains pending full runtime, persistence, management, and platform parity review.
- Exact path: `modules/steam/README.md`.
- Behavior/evidence: the prior sentence implied that every discovery revision advance stops an import. A newer valid discovery revision can continue the active import when the represented source fingerprint and settings epoch are unchanged. Changed fingerprint or settings epoch cancels before the next step; an already submitted catalog finish may complete, and a later complete scan reconciles current settings. The README now describes that lifecycle without changing runtime behavior.
- Worker: `steam_auto_sync_docs_fix`, verified from live session state as `gpt-6-luna/xhigh` (session `01a0d635-fc9c-70a3-ab22-e95fc350c984`), changed only the authorized README path and did not commit.
- Independent review: `steam_sync_readme_correction_review`, verified from live session state as `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), checked the correction against the candidate lifecycle and reported no findings.
- Checks: `git diff --check` passed. This was a documentation-only edit, so no Rust formatting command was needed. Rust tests, builds, and Clippy were not run in this lightweight worktree. Integration validation remains `cargo test -p amaterasu-steam` and `cargo clippy -p amaterasu-steam --all-targets -- -D warnings` in the reserved clean source checkout.
- Platform evidence: wording was verified against the state machine and pinned Vibeshine reference at `457237db7e57f9c4be21c35bb07e28d1c100eb7d`; no native Steam runtime or platform qualification was run.
- Remaining: complete `global:steam_auto_sync` parity still lacks persistence/editor review, complete representation of all Vibeshine fingerprint inputs, force-sync parity, and native Steam qualification. Keep the ID pending.
- Path ownership note: coordinator inbox grant `coordinator-luna05-lutris-export-profile-path-grant-20260925` temporarily transferred `crates/release/src/profiles.rs` and `crates/release/src/profiles/tests.rs` from L01 to L05 only for the `amaterasu.lutris -> amaterasu.steam` export mapping in both catalog profile variants. The mapping stays absent until the bounded catalog export action is ready. Other release-profile changes are outside the grant; do not assign these paths to another child.
- Next slice: implement and review the bounded, owner-scoped `global:lutris_include_steam` direct Steam-ID export across the owned catalog/Lutris modules, then apply the granted profile mapping and profile tests after the export contract is ready.

## Slice 23: export direct Steam IDs for Lutris duplicate suppression

- Base/code-candidate SHA: `0373ec3d76fe1f0242a5a3a37b43c9a684f8ed08` / `69d236a7214bb9b1f6b9a9644eed64a423e3c85e` (`feat(lutris): export direct Steam IDs from catalog`).
- Stable ID/scope: `global:lutris_include_steam`, catalog-backed direct Steam duplicate membership; remains pending profile authorization and integrated validation.
- Exact paths: `modules/catalog/src/protocol.rs`, `modules/catalog/src/pipeline.rs`, `modules/catalog/src/pipeline/tests.rs`, `modules/catalog/README.md`, `modules/lutris/src/steam_catalog.rs`, and `modules/lutris/README.md`.
- Behavior: added `amaterasu.catalog/1` action `export_steam_ids`, returning bounded pages of only canonical nonzero numeric Steam IDs with revision and opaque cursor metadata. The modern catalog path projects IDs while scanning collection rows under the pinned state revision rather than reading the metadata-only document; the legacy path remains read-only. Cursors use the `steam_ids` query tag and support bounded provider-marker keys containing valid spaces or Unicode. Lutris follows and validates pages, retries/cancels through its existing child-call state machine, and deduplicates IDs before suppressing Lutris imports. This matches the pinned Lutris policy that suppresses a Lutris Steam entry only if a direct catalog row already carries its Steam ID; an unselected Steam discovery result does not suppress it.
- Worker: `lutris_steam_id_export_consumer`, confirmed before edits from its live session state as `gpt-6-luna/xhigh` (session `01a0d63d-a0e6-7b80-aa95-2e0b633f600d`), changed only `modules/lutris/src/steam_catalog.rs` and its focused tests. The lead implemented the owned catalog protocol/scan/tests and both module READMEs.
- Independent review: `steam_sync_readme_correction_review`, `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), re-reviewed the complete API and consumer after the modern-scan and cursor fixes. No API/consumer findings remain. The reviewer confirmed a P1 integration blocker: the granted release export mapping is absent and the original requested `amaterasu.lutris -> amaterasu.steam` owner would be wrong because both catalog profile variants import Steam rows under `amaterasu.management`.
- Checks: `cargo fmt --check --package amaterasu-catalog`, `cargo fmt --check --package amaterasu-lutris`, and `git diff --check` passed. Rust tests/builds/Clippy were not run in this lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-catalog`, `cargo test -p amaterasu-lutris`, `cargo test -p amaterasu-release`, `cargo clippy -p amaterasu-catalog --all-targets -- -D warnings`, `cargo clippy -p amaterasu-lutris --all-targets -- -D warnings`, and `cargo clippy -p amaterasu-release --all-targets -- -D warnings`.
- Platform evidence: compared the duplicate-selection condition against pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d` (`src/lutris_sync_policy.cpp`) from the Windows checkout. No native Linux Lutris/catalog runtime or cross-platform qualification was run.
- Remaining: outbox 044 asks for a permitted child isolation/landing workflow; outbox 045 requests changing the granted export owner to the actual `amaterasu.management` catalog; outbox 046 asks whether Lutris may also use the sanitized `export_list` operation through the shared `export_principals` map or whether an operation-specific Steam-ID map should be added. No release profile mapping was edited. Keep `global:lutris_include_steam` pending these authorization/path decisions, clean-checkout test/Clippy results, full management persistence/editor review, and native platform qualification.
- Next slice: continue an independent assigned Steam setting while coordinator/L01 resolves the release-profile owner, capability, and landing workflow; then add the approved profile authorization and run the clean-checkout integration commands above.

## Slice 24: honor an unlimited Steam recent-age window

- Base/code-candidate SHA: `a983e5d9ba4da1238d18dd811d9d7f27a44413b2` / `5fd8a424d50ca32bf85e34d262d47b12ded70a99` (`fix(steam): ignore timestamps when recent age is zero`).
- Stable ID/scope: `global:steam_recent_max_age_days`, date filtering for recent installed Steam games; remains pending full settings, management, integration, and platform review.
- Exact paths: `modules/steam/src/catalog_sync.rs` and `modules/steam/README.md`.
- Behavior: when `recent_max_age_days` is zero, the provider now accepts every installed eligible game with nonzero `LastPlayed`, even when its timestamp is in the future. A positive age window still rejects future timestamps and entries beyond the inclusive cutoff. This matches the pinned selector's age-positive guard and the documented “0 = ignore play date” policy.
- Worker: `steam_recent_age_zero`, confirmed from live session state before edits as `gpt-6-luna/xhigh` (session `01a0d657-b2c5-7ed3-a195-5d249ec6f71e`), changed only the two owned paths and added a focused zero-versus-positive age regression.
- Independent review: `steam_sync_readme_correction_review`, `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), found no issues. It confirmed the future and old timestamp checks short-circuit for zero age, apply for positive age, preserve the exact cutoff, and cannot underflow. Existing selection tests cover older-than-window behavior; the new test covers future timestamps. The exact cutoff equality does not have a dedicated assertion.
- Checks: worker and lead ran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; both passed. Rust tests/builds/Clippy were not run in the lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-steam` and `cargo clippy -p amaterasu-steam --all-targets -- -D warnings`.
- Platform evidence: compared against pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d` in `src/steam_sync_policy.cpp`; no native Steam runtime or cross-platform qualification was run.
- Remaining: a focused exact-cutoff assertion is recorded in Slice 27; it still needs execution in the reserved clean checkout. Management persistence/editor behavior, complete end-to-end selection evidence, and Steam runtime/platform qualification remain. Keep `global:steam_recent_max_age_days` pending coordinator validation.
- Next slice: the duplicate Steam exclusion behavior is recorded in Slice 26 below, the exact positive-age cutoff regression is in Slice 27, and catalog export authorization and profiles are recorded in Slice 25.

## Slice 25: scope Lutris Steam-ID export authorization

- Base/auth-candidate SHA: `f9b1095b721c47c7a17c1f137adc38f1eba5e341` / `555ad5615d4d5ecb7d201738a0a8f137e840c11d`; isolated profile-worker base/candidate: `555ad5615d4d5ecb7d201738a0a8f137e840c11d` / `c45ea7dcdf716a29f05d889355a646c71a31cc66`; integrated lane profile commit: `6d57d67f65f732797cb34c3f9d9dc20136e823ea`.
- Stable ID/scope: `global:lutris_include_steam`, direct Lutris Steam-ID membership export and its startup authorization; remains pending coordinator integration gates and full end-to-end parity review.
- Exact paths: `modules/catalog/src/lib.rs`, `modules/catalog/src/pipeline.rs`, `modules/catalog/src/pipeline/tests.rs`, `modules/catalog/README.md`, `modules/lutris/README.md`, `crates/release/src/profiles.rs`, and `crates/release/src/profiles/tests.rs`.
- Authorization: after the coordinator amended the grant, the catalog has a distinct `steam_id_export_principals` map from immediate caller `amaterasu.lutris` to fixed owner `amaterasu.management`. Generic `export_principals` remains exactly `amaterasu.nvhttp -> amaterasu.management`; the Lutris caller cannot use `export_list`. Catalog tests cover map isolation, fixed owner despite a different initiator, owner-spoofing denial, and the absence of generic export access. Development and installer profile tests assert both exact maps.
- Profile worker: `lutris_export_profile_worker`, confirmed `gpt-6-luna/xhigh` (session `01a0d668-3122-72c0-a64b-d2bbb031c443`), edited only the two granted release files in an isolated worktree and committed `c45ea7dcdf716a29f05d889355a646c71a31cc66`. The lead reviewed the exact diff and cherry-picked it as `6d57d67f65f732797cb34c3f9d9dc20136e823ea`.
- Independent review: `steam_sync_readme_correction_review`, `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), found no issues in the profile grant or exact-map assertions.
- Checks: profile worker ran `cargo fmt --check -p amaterasu-release` and `git diff --check`; after lane integration the lead ran `cargo fmt --check --package amaterasu-catalog --package amaterasu-release` and `git diff --check`. Rust tests/builds/Clippy were not run in this lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-catalog`, `cargo test -p amaterasu-lutris`, `cargo test -p amaterasu-release`, and strict Clippy for each of those packages.
- Platform evidence: profile serialization and map policy were checked in the Windows checkout; no native Lutris/Linux or release runtime qualification was run.
- Remaining: the profile authorization is now present, but clean-checkout catalog/Lutris/release gates, management persistence/editor parity, full end-to-end operation wiring, and native runtime/platform qualification remain. Keep `global:lutris_include_steam` pending coordinator review.
- Next slice: the exact-cutoff regression is recorded in Slice 27. Continue an independent assigned application/provider setting after the coordinator receives this lane update.

## Slice 26: accept duplicate Steam game exclusions

- Base/candidate SHA: `555ad5615d4d5ecb7d201738a0a8f137e840c11d` / `e14cd7076cf0647eebee53b756631e3db402adf5` (`fix(steam): allow duplicate game exclusions`).
- Stable ID/scope: `global:steam_exclude_games`, repeated exclusion IDs and names; remains pending full settings/editor and provider parity review.
- Exact paths: `modules/steam/src/settings.rs`, `modules/steam/src/tests.rs`, and `modules/steam/README.md`.
- Behavior: duplicate ID and name exclusions are accepted and retained in order for typed settings, legacy JSON-array strings, comma-separated strings, and management updates. Per-entry validation and the current 256-entry limit remain. This matches the pinned parser's append-each-entry behavior; repeated exclusion checks are inert after the first match.
- Worker: reused `steam_enabled_slice` after its earlier task completed; live session `01a0d616-769c-74b1-accf-505a015d5c98` confirmed `gpt-6-luna/xhigh`. It changed only the two Rust paths; the lead documented the behavior and committed the reviewed slice as `e14cd7076cf0647eebee53b756631e3db402adf5`.
- Independent review: `steam_sync_readme_correction_review`, `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), found no findings and confirmed the pinned append-and-first-match behavior.
- Checks: worker and lead ran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; both passed. Rust tests/builds/Clippy were not run in the lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-steam` and `cargo clippy -p amaterasu-steam --all-targets -- -D warnings`.
- Platform evidence: behavior was compared with pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`; no native Steam runtime or cross-platform qualification was run.
- Remaining: name-only exclusion validation, per-entry validation details, and the 256-entry bound have not been fully reconciled with the reference parser. Management persistence/editor behavior and Steam runtime/platform qualification also remain open. Keep `global:steam_exclude_games` pending.
- Next slice: the exact positive-age cutoff regression is recorded in Slice 27. Continue an independent assigned application/provider setting after this test-only correction.

## Slice 27: assert the inclusive Steam recent-age boundary

- Base/worker-candidate/integrated-candidate SHA: `6d57d67f65f732797cb34c3f9d9dc20136e823ea` / `e80d0c07e379fecece64e76e1d15782f3afbfc91` / `b349c1701e65bc3f3f0244dc56c6ceb4614d778d` (`test(steam): cover inclusive recent age cutoff`).
- Stable ID/scope: `global:steam_recent_max_age_days`, positive age cutoff equality; the ID remains pending full selection, persistence, management, and platform parity.
- Exact path: `modules/steam/src/catalog_sync.rs`.
- Behavior/evidence: production selection already uses the inclusive boundary required by the pinned selector. The new deterministic regression sets a one-day age limit and a game timestamp exactly 86,400 seconds before `now`, then asserts that its catalog record remains selected. No production behavior changed.
- Worker: `steam_age_boundary`, confirmed `gpt-6-luna/xhigh` before edit (session `01a0d66d-f300-7d60-bcd0-3259579bea25`), added only this assertion in its isolated worktree and committed `e80d0c07e379fecece64e76e1d15782f3afbfc91`. The lane lead reviewed the exact file and cherry-picked it as `b349c1701e65bc3f3f0244dc56c6ceb4614d778d`.
- Independent review: `steam_sync_readme_correction_review`, confirmed `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), found no findings; it confirmed that the fixed timestamp deterministically distinguishes `<=` from `<`.
- Checks: worker ran `cargo fmt --check --package amaterasu-steam` and `git diff --check`; both passed. Rust tests/builds/package/Clippy were not run in this lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-steam` and `cargo clippy -p amaterasu-steam --all-targets -- -D warnings`.
- Platform evidence: the boundary is integer timestamp arithmetic and was compared with pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`; no native Steam runtime or cross-platform qualification was run.
- Remaining: execute the new regression with the full Steam package suite, complete the management persistence/editor audit, and qualify Steam runtime/platform behavior. Keep `global:steam_recent_max_age_days` pending.
- Next slice: continue an independent assigned application/provider setting and report any cross-lane contract dependencies through the outbox.

## Coordinator validation update: Steam main gates and exclusion conflict

- Source commits `47604651b267ee00a197bb34e81e926a6e5665de`, `be049899051f7ca61b65332bae6b9c1c3f790c1e`, `c7672084b25c22308cc899f9781a2523047cda42`, and `9256d862f938f1183e92f71504b7731b3120fa1c` landed on main as `06cf2e0`, `3e864347`, `f05214fa`, and `0f0ec486`. The coordinator reports package format passed, `cargo test -p amaterasu-steam` passed 67/67 on Windows, strict Clippy passed, and `git show --check` passed for the documentation commit.
- Source commit `5fd8a424d50ca32bf85e34d262d47b12ded70a99` landed on main as `e7faf94e`; the coordinator reports format passed, Steam tests passed 68/68 on Windows, and strict Clippy passed.
- These main gates validate the listed source commits only. The Slice 27 inclusive-boundary assertion and Slice 26 duplicate acceptance still need clean-checkout execution.
- Steam promotion blocker: the lane retains `0d54977c`'s paired ID-or-name matching and its `paired_exclusions_match_by_id_or_name_during_catalog_selection` regression. Main commit `0f0ec486` uses ID-priority with name fallback only when ID is absent. The pinned Vibeshine selector at `457237db7e57f9c4be21c35bb07e28d1c100eb7d` checks both nonempty ID and name alternatives independently, so this creates a direct main-versus-parity contract conflict for `global:steam_exclude_games`.
- No main files were edited. Before any further Steam promotion, coordinator decision is requested on whether to preserve main's ID-priority behavior (and leave the pinned parity difference explicit) or preserve the pinned ID-or-name behavior and resolve the existing main divergence. Keep all Steam IDs pending; the coordinator owns main changes and integration.

## Slice 28: accept duplicate Playnite game exclusions

- Base/worker-candidate/integrated-candidate SHA: `ed69e351c96161a14891bdca3a1835b40e1db348` / `f70776affcad065aa1adce21d3972ef07f056e1e` / `5636c4458107bdc9603bfb53994423ca97343e2b` (`fix(playnite): retain duplicate game exclusions`).
- Stable ID/scope: `global:playnite_exclude_games`, repeated excluded Playnite game IDs; remains pending full persistence/editor and Windows provider qualification.
- Exact paths: `modules/playnite/src/settings.rs`, `modules/playnite/src/tests.rs`, and `modules/playnite/README.md`.
- Behavior: repeated game IDs, including ASCII case variants, are now accepted and retained in order by validation, product-document persistence, versioned update payload serialization, and management update decoding. Selection already folds configured IDs into a set, so repeated matches remain inert. Per-entry nonempty/512-byte/control-character checks, the 256-entry limit, and the total settings-size bound remain. Category uniqueness is unchanged; plugin duplicate behavior is unchanged.
- Worker: `playnite_game_exclusions`, confirmed `gpt-6-luna/xhigh` before edits (session `01a0d674-8a1e-7ef3-b91a-273d905d2cb6`), changed only the three exact paths in an isolated worktree and committed `f70776affcad065aa1adce21d3972ef07f056e1e`. The lane lead reviewed and cherry-picked it as `5636c4458107bdc9603bfb53994423ca97343e2b`.
- Independent review: `steam_sync_readme_correction_review`, confirmed `gpt-6-luna/xhigh` (session `01a0d638-7723-7332-ab8f-d4a35095280e`), found no findings. It verified category uniqueness and plugin behavior remain unchanged, bounds are preserved, and the pinned parser retains repeated nonempty entries.
- Checks: worker ran `cargo fmt --check --package amaterasu-playnite` and `git diff --check`; both passed. Rust tests/builds/Clippy were not run in this lightweight worktree. Clean-checkout integration commands: `cargo test -p amaterasu-playnite` and `cargo clippy -p amaterasu-playnite --all-targets -- -D warnings`.
- Platform evidence: parser behavior was compared with pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`; no native Playnite Windows runtime qualification was run.
- Remaining: end-to-end editor presentation/persistence and Windows Playnite runtime qualification remain. Amaterasu retains excluded game IDs as strings and does not preserve the reference parser's optional `{id,name}` display metadata. Keep `global:playnite_exclude_games` pending.
- Next slice: continue an independent assigned application/provider setting while the coordinator resolves the Steam exclusion contract conflict in Outbox 052.

## Slice 40: prove explicit AV1 auto survives provider refresh

- Worker base / worker correction commit / integrated lane correction commit: `5e54f6197e539f548c77eded98b590919ea70eab` / `9e1decf4029944d81d8390279e208478e56b19b1` / `618648eacfcbf3f89b870dd07433cf6ee978e1fe`.
- Stable IDs/scope: cross-lane `global:amd_av1_latency_mode`; L05 `per_application:config-overrides`. Both remain pending full parity validation.
- Exact path: `modules/catalog/src/pipeline/tests.rs` only.
- Correction: the existing `provider_snapshot_preserves_manual_config_overrides_across_later_refreshes` test now stores explicit `amd_av1_latency_mode: "auto"` on the manually edited entry and asserts the same value survives the next provider snapshot refresh. Previous name, exclusion, FEC, audio capture, back-button, provider-marker, finish, and cleanup assertions remain.
- Worker: `catalog_pipeline_constructor_cleanup`, session `01a0d680-1c87-7212-a809-a32f0aafb875`, verified `gpt-6-luna/xhigh`, committed the test in an isolated worktree. The lead reviewed and cherry-picked it as `618648eacfcbf3f89b870dd07433cf6ee978e1fe` after the AV1 implementation commit `5e54f6197e539f548c77eded98b590919ea70eab`.
- Independent review: `detached_env_reviewer`, session `01a0d6ac-89ab-71d1-9e02-5a5717568088`, verified `gpt-6-luna/xhigh`, reviewed candidate `9e1decf4029944d81d8390279e208478e56b19b1` against its base `5e54f6197e539f548c77eded98b590919ea70eab`. No findings; the reviewer confirmed the explicit-auto value is asserted after refresh and all prior assertions remain.
- Checks in the lane: worker and lead ran `cargo fmt --check --package amaterasu-catalog` and `git diff --check`; all passed. Tests/build/Clippy did not run in the lightweight worktree.
- Platform evidence: no native AV1/AMF runtime or platform qualification was run.
- Remaining: keep both IDs pending global/client/application resolution, strict NVHTTP consumer and nested video serialization (L02), unsupported-backend side-effect behavior (L03), management persistence/editor integration, and native AMF qualification. This test correction closes the review coverage gap only.
- Next slice: complete the separately reserved Steam canceled-Finish correction and its independent review; do not mark AV1 or config-overrides verified from this projection-only work.

## Coordinator validation update: AV1 application projection

- Source commits `5e54f6197e539f548c77eded98b590919ea70eab` and `618648eacfcbf3f89b870dd07433cf6ee978e1fe` landed on main as `8aaeb4f` and `f22bc88`. The first conflict was resolved by retaining main's existing models and applying only the AV1 field, validation, tests, and scoped documentation; unrelated lane fields were excluded.
- Current-source gates: `cargo fmt --check -p amaterasu-catalog -p amaterasu-launch` passed; catalog tests passed 69/69 and strict Clippy passed; launch tests passed 63 with 2 subprocess tests ignored and strict Clippy passed.
- Main's launch test harness predates the lane-only override helper. Coordinator commit `344a0d1` added typed AV1 fixture support; the launch retry/status and invalid-input tests now run against the main harness.
- Both stable IDs remain pending global/client/application resolution, L02 nested video consumer projection, L03 fail-closed backend behavior, management persistence/editor integration, and native AMF qualification.

## Slice 42: pin the inclusive Playnite recent-age cutoff

- Worker base / worker candidate / integrated lane commit: `fa6ffd6552a112bd2be2546edc4af57bf314992e` / `938246c2d600e623202478c45ab4569f4a77aeaf` / `81a2903d69468c5886f5802f1a9da10bcf57e213` (`test(playnite): pin inclusive recent age cutoff`).
- Stable ID/scope: `global:playnite_recent_max_age_days`, positive cutoff equality; it remains pending full settings, management, runtime, and Windows qualification. Exact path: `modules/playnite/src/snapshot.rs` only.
- The pinned `select_recent_installed_games` excludes only timestamps strictly below `now - age_days * 86,400`, so equality is included. Amaterasu already uses `last_played >= cutoff`; production behavior is unchanged. The regression selects a game at exactly 30 days before `now` and rejects a game one second older.
- Worker `playnite_recent_age_boundary` used isolated worktree `D:/sources/worktrees/amaterasu-luna05-playnite-age-boundary-xhigh`, exact base `fa6ffd6552a112bd2be2546edc4af57bf314992e`, and committed `938246c2d600e623202478c45ab4569f4a77aeaf`. The lead reviewed its exact path and diff, then integrated it as lane commit `81a2903d69468c5886f5802f1a9da10bcf57e213`. Independent XHigh review found no findings and confirmed the timestamps isolate equality from the one-second-old case.
- Worker and lead passed `cargo fmt --check --package amaterasu-playnite` and `git diff --check`. Rust tests/build/Clippy were not run in the lightweight worker.

## Coordinator validation update: Playnite recent-age cutoff

- Lane commit `81a2903d69468c5886f5802f1a9da10bcf57e213` landed on main as `ba20eae7`. Main formatting passed, Playnite tests passed 81/81, and strict Clippy passed. No production behavior changed.
- `global:playnite_recent_max_age_days` remains pending settings/editor persistence and Windows provider qualification.

## Coordinator validation update: detached command environment expansion

- L05's reviewed main-based commits `bdc78031ba9af4eb9f176311f1517e9485d75c60` and `3e6321a15a87f46266ccbca7775a9f55d34c1422` landed directly as `643771552a73dae144ea88c9ef771d4c26ab5e07` and `4bcae0d97acdc13eee2d488bdac8c4bc2a20c99d`. The lane's `28faa9495ff742f7668df4679e4b9fe00b97b165` implementation uses a separate lifecycle API; L05 supplied an equivalent main-based candidate reviewed against the current application API.
- Main validation passed `cargo fmt --check --package amaterasu-application`, `git diff --check`, 51 application tests (one subprocess entry point ignored), and strict Clippy.
- `per_application:detached` remains pending full management/editor persistence and signed readback, exact child working-directory/environment parity, and native platform qualification. Output redirection, per-command environment overrides, and elevation remain unimplemented in this slice.
