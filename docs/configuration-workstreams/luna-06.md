# Luna-06 configuration parity handoff

## Candidate and session verification

- Repository: D:/sources/worktrees/amaterasu-config-management
- Branch: config-parity/management
- Source baseline: f220bcaa4a9d4c28e410a877b0809c14d08bb88b
- Lane starting SHA: 30acd5de34ac5548ebf4d46086fc32ccad7b6913
- Previous lane base: 3fccc2f4628bad07c9cf22b933254a80b1c0aa1c
- Diagnostics code candidate reviewed: 8c45a2b1029ab425bcefed5630328ca111164ad6
- Lane head before readiness documentation: aa41464f666a8a23d88055fdbfeff3f1ec68a310
- Readiness documentation commit: 9e60093380fe4343680a8774f9ffbb5a46993f65
- Isolated v17 editor worker commit: 122354ffd01d4190b212dd5ea1892dec6990a37f
- Integrated v17 editor lane commit: cf0ad6da338ddc23fddaceae0335b9407be6430d
- Isolated diagnostics compile-fix worker commit: 4a71e6a53695d90975c60d19bc9b560d319e587a
- Integrated diagnostics compile-fix lane commit: 16544bb945d600f905eee9016662f720734d7306
- Coordinator main head at the priority-fix checkpoint: 4911c9b9; provider grant/dependency commits: 687f72a2, b084b0e7
- Pinned Vibeshine reference: 457237db7e57f9c4be21c35bb07e28d1c100eb7d
- Session: 01a0d516-8960-7e02-ac81-3eee350f679a, gpt-6-luna / max.
- Startup verified repository root, branch, starting HEAD, baseline, read/write access, and native subagent support. Two settings implementation workers were confirmed as gpt-6-luna / xhigh before their edits.
- No remote push, merge, or shared-baseline change was made.

## Completed bounded slice: management settings and preference services

Code commit 15d114a adds cookie-backed management authentication, host-configuration and settings route validation, management editors, cross-platform-safe frame-limiter editing, and three revision-CAS settings services.

The host-metrics service stores the nine assigned realtime statistics preferences in a version-1 product document. The updater service stores notify_pre_releases and update_check_interval. The system-tray service stores system_tray. All three default from their pinned source behavior on an absent document, validate updates, restrict their settings route to management, and verify the committed configuration revision and values.

The management frontend exposes host path/logging, session-history, realtime statistics, updater, tray, and catalog settings. Global and catalog frame-limiter parsers preserve unsupported values and require explicit clearing where needed. The existing session-history editor and product-settings code expose enabled, ttl_days, and db_size_limit_mb with version-1 persistence and migration from a prior SQLite settings row. The earlier committed diagnostics/history support remains at 8ae7027; its fatal log severity and bounded ended-session pruning are supporting changes, not coverage evidence for these assigned IDs.

Exact paths in code commit 15d114a:

- frontend/README.md
- frontend/src/api.ts
- frontend/src/app.ts
- frontend/src/catalog.ts
- frontend/src/settings.ts
- frontend/src/types.ts
- frontend/tests/api.test.mjs
- frontend/tests/catalog.test.mjs
- frontend/tests/settings.test.mjs
- modules/host-metrics/Cargo.toml
- modules/host-metrics/README.md
- modules/host-metrics/src/lib.rs
- modules/host-metrics/src/pipeline.rs
- modules/host-metrics/src/preferences.rs
- modules/host-metrics/src/protocol.rs
- modules/management/README.md
- modules/management/src/http.rs
- modules/management/src/route_payload.rs
- modules/management/src/routes.rs
- modules/management/src/server.rs
- modules/system-tray/Cargo.toml
- modules/system-tray/README.md
- modules/system-tray/src/lib.rs
- modules/system-tray/src/pipeline.rs
- modules/system-tray/src/protocol.rs
- modules/system-tray/src/settings.rs
- modules/updater/Cargo.toml
- modules/updater/README.md
- modules/updater/src/lib.rs
- modules/updater/src/pipeline.rs
- modules/updater/src/preferences.rs
- modules/updater/src/protocol.rs

## Completed bounded slice: persisted diagnostics logging policy

Code commit 8c45a2b adds an asynchronous diagnostics consumer for the persisted
`amaterasu.host-configuration/1` v1 document. Diagnostics continues opening its
trusted bootstrap logging policy during startup, then reads the settings service
without blocking its request poll. Worker `READY` is not gated on persisted
logging hydration. It applies only ready revisions above zero, keeps the
bootstrap policy while the document is absent or unavailable, and refreshes
changed revisions every 30 seconds. The reader releases terminal
child results, preserves pending releases after transport failures, retries a
lost submit acknowledgement with the same operation ID, and validates the
stored path through the existing trusted state root. Only diagnostics records
use this sink; host and native library logs are not redirected.

The active bootstrap sink remains in use until a valid ready persisted revision
above zero is adopted. A new persisted revision reconfigures and reopens the
sink. A disk sink failure for the active revision is not retried automatically;
the reader only reapplies changed revisions.

Unit coverage was added for the async request/result lifecycle, result release
retry, revision-zero fallback, lost-ack operation ID reuse, level mapping,
stored path preservation, malformed settings, and unrelated files in a custom
log directory. Rust tests were not run in this lightweight worktree.

The assigned log settings fields already exist in host-configuration v1. The
host release-profile grant and diagnostics dependency landed on main in commits
`687f72a2` and `b084b0e7`, so the service call is available there. Coordinator
integration will validate this consumer with the provider.

Exact paths in code commit 8c45a2b:

- modules/diagnostics/README.md
- modules/diagnostics/src/host_logging.rs
- modules/diagnostics/src/journal.rs
- modules/diagnostics/src/lib.rs
- modules/diagnostics/src/logging.rs

## Completed bounded slice: v17 encryption and frame-limiter editor projection

The isolated management editor worker used worktree
`D:/sources/worktrees/luna06-v17-editor-xhigh`, branch
`codex/luna06-v17-editor-xhigh`, and session
`01a0d5cc-8124-7441-b1d6-5329147af6cd`. Its verified settings were
`gpt-6-luna / xhigh`. It committed `122354ffd01d4190b212dd5ea1892dec6990a37f`;
the exact tree was reviewed read-only in isolated worktree
`D:/sources/worktrees/luna06-v17-editor-review` by a verified
`gpt-6-luna / xhigh` reviewer, with no findings. The reviewed worker commit was
cherry-picked onto lane head `9e60093380fe4343680a8774f9ffbb5a46993f65` as
`cf0ad6da338ddc23fddaceae0335b9407be6430d`.

The slice covers global `lan_encryption_mode` and `wan_encryption_mode`, plus
the global, application, and paired-client frame-limiter editor fields from the
L02/L03/L05 contract. Encryption selects encode 0=never, 1=opportunistic, and
2=mandatory, with LAN=0 and WAN=1 defaults, while preserving the existing
signed `expected_revision` management update and `/v1` API. Frame-limiter FPS
uses canonical decimal-Hz `frame_limiter_fps_limit` from 0 through 1000 with
three fractional digits maximum. Sparse app/client values preserve null or
absence for inheritance and explicit false/zero; the legacy VSync alias is
normalized and conflicts with the canonical key are rejected. RTSS install and
virtual-display settings and MangoHUD settings stay global-only.

L02's coordinator-confirmed version decision is one NVHTTP v16-to-v17 batch;
no v18 is introduced. The editor currently rejects null encryption modes and
uses typed readback fields `configured_lan_encryption_mode` and
`configured_wan_encryption_mode`. L02 still needs to confirm those readback
keys/shape and whether null is a reset operation. The editor does not send null
for either mode, so no reset behavior is assumed. L02 remains the only
persistence writer. Frame-limiter storage, resolved precedence, launch
snapshot construction, and runtime behavior remain L02/L03/L05 work.

Exact paths in integrated commit `cf0ad6d`:

- frontend/src/catalog.ts
- frontend/src/settings.ts
- frontend/src/types.ts
- frontend/tests/catalog.test.mjs
- frontend/tests/settings.test.mjs
- modules/management/src/route_payload.rs

## Latest checks and evidence

Passed on the lane candidate `cf0ad6da338ddc23fddaceae0335b9407be6430d`:

- `npm test` — TypeScript typecheck and all 61 Node tests passed.
- `rustfmt --edition 2024 --check modules/management/src/route_payload.rs`.
- `git diff --check`.

The read-only editor review reported no findings for enum validation/defaults,
decimal-Hz bounds, sparse inheritance, false/zero fidelity, alias handling,
global-only field scope, signed revision CAS, and payload tests. No browser,
runtime, or platform evidence was collected. Rust tests/build/Clippy remain
coordinator-only and were not run in this lightweight lane.

## Priority diagnostics test-compile correction

The coordinator reported that main commit `ced4c8cc` failed to compile in
`modules/diagnostics/src/logging.rs`: the retention test moved `root` into
`fs::read_dir(root)` and later reused it in `root.join(filename)`. A verified
isolated worker used worktree
`D:/sources/worktrees/luna06-diagnostics-compile-fix-xhigh`, branch
`codex/luna06-diagnostics-compile-fix-xhigh`, based on lane head
`bf5b83228b0243ae0ca1c97262619b049558be96`. Worker session
`01a0d5f2-a755-72b2-89ed-f69a9e480d25` was verified as `gpt-6-luna / xhigh`.

The child commit is `4a71e6a53695d90975c60d19bc9b560d319e587a`; read-only lead
review confirmed its only change is `fs::read_dir(&root)` in the affected
test. It was cherry-picked onto the lane as
`16544bb945d600f905eee9016662f720734d7306`. Only
`modules/diagnostics/src/logging.rs` changed. The worker and lane passed
`rustfmt --edition 2024 --check modules/diagnostics/src/logging.rs` and
`git diff --check`. Cargo tests/build/Clippy were not run here. The coordinator should run
`cargo test -p amaterasu-diagnostics` and
`cargo clippy -p amaterasu-diagnostics --all-targets -- -D warnings` in the
clean integration checkout.

This is only a compile correction. The separate restart-safe operation-ID
replay follow-up remains pending and is not addressed by this commit.

## Priority diagnostics strict-Clippy correction

The coordinator reported `clippy::op_ref` at
`modules/diagnostics/src/logging.rs:381` on main `278ff6d3`. The compiler
requested comparing `rollover_index.to_string()` directly with
`rollover[1..]`; the fix removes only the RHS reference and does not alter
rollover validation behavior.

An isolated `gpt-6-luna/xhigh` worker at session
`01a0d617-0233-7e61-8ea5-51b463fe86c7` used worktree
`D:/sources/worktrees/luna06-diagnostics-clippy-xhigh`, branch
`codex/luna06-diagnostics-clippy-xhigh`, based on the exact lane head
`a454324a165709abea776ece9d3f7282ae0a6b7a`. Its one-file commit was
`35547b7a32e1413226691a435403df0726e61ae3`. Lead review confirmed the only
change was the requested RHS reference removal, and it was cherry-picked as
lane commit `925213d8d86bf3e7be9d49042d2f9f60e4063a45`. Only
`modules/diagnostics/src/logging.rs` changed.

The worker and lane passed `rustfmt --edition 2024 --check
modules/diagnostics/src/logging.rs` and `git diff --check`. Cargo tests and
strict Clippy were not run in this lightweight worktree. The coordinator
should rerun `cargo test -p amaterasu-diagnostics` and
`cargo clippy -p amaterasu-diagnostics --all-targets -- -D warnings` in the
clean integration checkout. The operation-ID replay follow-up remains a
separate change.

## Completed bounded diagnostics operation-ID replay follow-up

The review of diagnostics candidate `8c45a2b1029ab425bcefed5630328ca111164ad6`
found that its process-local request counter reset on restart. A restarted
reader could not reattach to an outstanding request whose ID suffix was above
zero. The reader now uses the stable caller-scoped key
`diagnostics.host-configuration.logging.read` for its serialized GET lifecycle.
Its request service, payload, and parent remain constant, so the broker returns
the retained request ID while that request record exists. The reader releases
each terminal result before submitting a later refresh; release removes the
broker retry record and permits reuse of the stable key. No document, API, or
release-profile schema version was changed.

The verified implementation worker was `gpt-6-luna/xhigh`, session
`01a0d613-6259-7720-b96c-8bc7db7f8cf2`, worktree
`D:/sources/worktrees/luna06-diagnostics-replay-xhigh`, branch
`codex/luna06-diagnostics-replay-xhigh`, based on
`a454324a165709abea776ece9d3f7282ae0a6b7a`. Worker commit
`0cc35c3a12565e0e1100388ea29e73fa35ae3014` changes only
`modules/diagnostics/src/host_logging.rs`; after lead review it was
cherry-picked as lane commit `83ca12660d5b2ec7f05a175816f76d585ae62bda`.

The independent read-only review found no production correctness issue in the
stable-key lifecycle. It confirmed that broker retries require the same caller,
operation ID, service, payload, and parent; `ReleaseResult` deletes the retry
record; completed records expire after 300 seconds; and a broker restart loses
its in-memory retry table. The new restart tests are diagnostics unit
simulations: their closures model reattachment, completion, and release; they
do not instantiate the foreign-owned runtime broker. Full broker integration
remains an integration-checkout evidence gap. The stable key also assumes one
diagnostics reader per caller; simultaneous readers would share a request and
could race. On the first upgrade from numbered IDs, any old-version request
cannot be attached using the new key and remains only until normal broker
expiry.

The worker and lane passed `rustfmt --edition 2024 --check
modules/diagnostics/src/host_logging.rs`, `git diff --check`, and committed
diff checks. Rust tests/build/Clippy were not run in this lightweight
worktree. Coordinator inbox `coordinator-diagnostics-operation-replay-contract-ack.json`
accepted the caller-scoped stable ID for one serialized reader while the same
in-memory runtime broker remains alive; no document, API, or release-profile
schema reservation is needed. Main integrated the strict-Clippy correction as
`c31f741b`, then the replay candidate as `6a7d6d1b`. After replay integration,
the coordinator passed rustfmt, `cargo test -p amaterasu-diagnostics`
(19/19), and strict Clippy with
`cargo clippy -p amaterasu-diagnostics --all-targets -- -D warnings`.

The accepted contract retains material lifecycle limits: restarting the
runtime/broker clears its in-memory request table, and simultaneous readers
sharing the same caller and operation ID can race. The restart tests are unit
simulations, not integration with the runtime broker. No platform/runtime
qualification is established by the coordinator's package tests. Keep
`global:log_path` and `global:min_log_level` pending until integrated consumer,
authority, runtime, and platform evidence is complete; do not edit L01 release
profile paths.

## Completed bounded slice: sparse video stream override editor routes

The `luna-02-stream-overrides-01` relay assigned L06 the management/UI portion
of `global:max_bitrate`, `global:minimum_fps_target`, and
`global:video_max_batch_size_kb` at application and paired-client scopes. The
contract keeps absent/null values inheriting, preserves explicit zero, accepts
signed i32 bitrate, finite FPS in 0..=1000, and integer batch sizes in 0..=64.
Catalog app writes use the existing `catalog.write` authority and
`id`/`expected_revision` CAS. Paired-client writes use the existing
`pairing.admin` authority, paired `certificate_id`, and `expected_revision`;
caller-supplied client identity is rejected. `/v1` is unchanged.

The catalog editor writes app values inside `entry.config_overrides` through
`catalog/create` or `catalog/update`. The provider override editor and paired
client settings editor retain their existing sparse `config_overrides`
endpoints. The host batch-size helper now explains that 16/32/64 apply directly
and other whole values from zero through 64 normalize to 64 KiB. Runtime
normalization remains Luna02's responsibility.

Verified isolated implementation worker: `gpt-6-luna/xhigh`, session
`01a0d5fe-2c55-76a0-9959-1e402f3d4c81`, worktree
`D:/sources/worktrees/amaterasu-stream-overrides-xhigh`, branch
`codex/luna06-stream-overrides-xhigh`, based on
`b53e6fcbd3456d65a25e89f6c0d6b5f1e9a530be`. Its commits were:

- `2aba0ba9f43d7c1efadc7c7d262cb0cd639460d8`: sparse, bounds, null/zero,
  revision, and paired-certificate route validation coverage plus the batch
  size help text.
- `37e03ee674f31aff7767ad3c3fd877e3a0e513c1`: nested `catalog/update`
  coverage for the actual application editor payload, including the app ID,
  expected revision, valid/null/zero values, and rejected types/ranges.

After lead review, these were cherry-picked as lane commits
`50986a7a6f045699188ca62daca93d48bd8a19b4` and
`00457c9cc92fe35466446e36ca64380efa09f15a`, respectively. The only changed
paths are `modules/management/src/route_payload.rs` and
`frontend/src/settings.ts`; `frontend/src/catalog.ts` was inspected and
remained unchanged. No catalog persistence or migration path was edited.

The lane passed `rustfmt --edition 2024 --check
modules/management/src/route_payload.rs`, `git diff --check`, and `npm test`
(TypeScript typecheck plus all 61 frontend tests). Rust tests/build/Clippy were
not run in this lightweight worktree. The coordinator should run
`cargo test -p amaterasu-management -p amaterasu-catalog -p amaterasu-nvhttp`
and `cargo clippy -p amaterasu-management -p amaterasu-catalog -p
amaterasu-nvhttp --all-targets -- -D warnings` in the clean integration
checkout. No browser, runtime, or OS-platform qualification was collected.

Outbox `0025-luna02-stream-overrides-management-ack.json` accepts only the
three assigned IDs/scopes and proposes catalog `Entry.config_overrides` schema
version 2 as an additive optional-field migration. Outbox
`0026-luna02-stream-overrides-worker-contract.json` records the exact worker
paths, verified worker session, route authorization, and frontend persistence
contract before the follow-up edit. Coordinator/L05 confirmation of the
schema-version proposal remains pending; do not edit Luna05 persistence paths.
Luna02/L05 still own cross-restart persistence, client > application > global
precedence, per-stream snapshots, and concurrent-stream isolation. All three
IDs remain pending, not verified.

## Review and checks

An independent read-only reviewer checked updater, tray, and host-metrics settings/CAS handling and the tick-level tests. The review found an updater child-result release defect and a test-order assertion gap. The updater now releases retained terminal TakeResult errors before completing the outer request, and the tests assert release occurs before completion. The reviewer confirmed both findings were addressed and reported no further findings.

An earlier read-only diagnostics review found that custom-directory retention
could delete similarly prefixed unrelated files. The parser now recognizes only
generated timestamp/process/attempt labels and valid rollover names, and the
expanded regression test keeps unrelated files; that finding was confirmed
closed.

A separate verified read-only review examined exact candidate
8c45a2b1029ab425bcefed5630328ca111164ad6 in isolated worktree
`D:/sources/worktrees/luna06-review-diagnostics-8c45a2b1`. Reviewer session
`01a0d5c1-9d45-7531-bb6d-0135b7e99e54` reported actual model `gpt-6-luna`,
effort `xhigh`. Release retries, readiness/revision validation, trusted relative
paths, retention filtering, and bootstrap behavior had no further findings.
One operational gap remains: `next_operation_id` resets when the module
restarts. A pending or unreleased request with a later ID cannot be replayed by
the new instance and can remain in broker retention until expiry (up to five
minutes, within the broker's 512-request cap). The GET is read-only, so it does
not repeat a mutation, but repeated restarts with a stuck provider could consume
shared request capacity. No diagnostics code fix was made in this review update;
the next diagnostics follow-up should use a stable in-flight operation ID or
persist replay state and cover module restart with a pending request.

The diagnostics README now says worker `READY` is not gated on logging
hydration, bootstrap logging stays active until a ready positive revision is
adopted, and an active-revision sink failure is not retried automatically.

Passed in this lightweight worktree:

- frontend: npm test — TypeScript typecheck and 60 Node tests passed.
- rustfmt --edition 2024 --check on changed management, host-metrics, system-tray, updater, and diagnostics Rust sources.
- git diff --check.

Not run here, as required for this lightweight worktree:

- Rust tests, builds, packaging, and Clippy.
- Browser qualification for cookie behavior.
- Runtime qualification on Windows, Linux, and macOS.

Run from the coordinator's clean integration checkout after provider registration:

- cargo test -p amaterasu-management -p amaterasu-host-metrics -p amaterasu-system-tray -p amaterasu-updater -p amaterasu-session-history -p amaterasu-diagnostics
- cargo clippy -p amaterasu-management -p amaterasu-host-metrics -p amaterasu-system-tray -p amaterasu-updater -p amaterasu-session-history -p amaterasu-diagnostics --all-targets -- -D warnings
- Verify profile loading, provider grants, host startup consumers, settings migration, browser cookie behavior, and OS-specific tray/updater/metrics behavior.

## Workers and review

- Host-metrics settings worker: gpt-6-luna / xhigh; implemented the preference protocol, validation, CAS pipeline, documentation, and helper tests in modules/host-metrics.
- System-tray settings worker: gpt-6-luna / xhigh; implemented the preference protocol, validation, CAS pipeline, documentation, and helper tests in modules/system-tray.
- Cookie-auth worker: gpt-6-luna / xhigh; implemented cookie-backed server/frontend session handling and tests in the owned management/frontend paths.
- Independent reviewer: read-only review of the new settings pipelines and lifecycle tests; confirmed fixes to the updater release behavior and release-order assertions.
- Diagnostics reviewer (retention correction): read-only review; model/effort were not exposed.
- Diagnostics candidate reviewer: isolated read-only review of exact SHA 8c45a2b; verified `gpt-6-luna/xhigh`, session 01a0d5c1-9d45-7531-bb6d-0135b7e99e54; found module-restart operation-ID replay gap, with no edits.
- Lead session: gpt-6-luna / max, session 01a0d516-8960-7e02-ac81-3eee350f679a.

## Assigned IDs and remaining parity gaps

All 18 assigned IDs remain pending in the coordinator-owned coverage ledger:

global:locale; global:log_path; global:min_log_level; global:notify_pre_releases; global:realtime_stats_enabled; global:realtime_stats_history_retention_seconds; global:realtime_stats_max_history_points; global:realtime_stats_pause_when_hidden; global:realtime_stats_poll_interval_ms; global:realtime_stats_show_active_sessions; global:realtime_stats_show_host_charts; global:realtime_stats_show_host_stats; global:realtime_stats_show_session_history; global:session_history_db_size_limit_mb; global:session_history_enabled; global:session_history_ttl_days; global:system_tray; global:update_check_interval.

Remaining blockers and mismatches:

- global:locale has no management editor or locale consumer in Amaterasu. The pinned source accepts 22 locale values with default en; host-configuration currently has no locale field and the Amaterasu frontend has no translated message catalogs. Outbox 0016 requests the host-configuration and locale-consumer contract before extending its v1 document.
- global:log_path and global:min_log_level have management GET/UPDATE mappings and an editor; diagnostics asynchronously reads persisted v1 policy and refreshes changed revisions. The release-profile grant and diagnostics dependency landed on main in `687f72a2` and `b084b0e7`; ordinary host/native log sinks are not redirected. The stable operation ID now reattaches across a diagnostics restart while the same broker retains the request, but the broker's retry table is in memory and resets with the broker. Request 0014 asks Luna01/coordinator for registration, grants, and migration ownership; request 0017's diagnostics read grant is satisfied on main.
- global:notify_pre_releases and global:update_check_interval persist through updater settings_get/settings_update, but the host updater scheduler and notification code do not yet consume the saved preferences. Workspace/package and release-profile registration and migration remain pending.
- All nine global:realtime_stats_* values persist through the host-metrics service and have management editing, but telemetry sampling and chart/session-history view consumers are not connected. Workspace/package and profile registration and legacy settings migration remain pending.
- global:system_tray persists through settings_get/settings_update, but host startup/shutdown does not yet consume the saved value. Workspace/package and profile registration, migration, and supported-platform qualification remain pending.
- The three global:session_history_* values have settings UI, strict validation, central product settings and migration from the prior SQLite history-settings row. Stream samples and events are not yet collected, and full integration/platform checks remain pending.
- The management editor now sends canonical decimal-Hz frame-limiter FPS at global, application, and paired-client scopes. Luna02 persistence and client > application > global resolution, Luna03 service behavior, and Luna05 launch lifecycle consumption still need integration and runtime evidence; these editor changes alone do not establish end-to-end parity.
- LAN/WAN encryption editor fields now use values 0/1/2 and defaults LAN=0/WAN=1 in the coordinator-reserved v17 batch. Luna02 remains the persistence writer. Its exact configured readback shape and null-reset semantics remain pending; do not allocate v18.
- Cookie-backed authentication and secure-cookie behavior still need browser checks on supported same-origin loopback origins.
- The coordinator owns the coverage ledger and all IDs remain pending until its full evidence review.

## Dependencies and next slice

- Outbox 0013 requests Luna02's NVHTTP v17 frame-limiter settings contract and implementation.
- Outbox 0014 requests Luna01/coordinator host-configuration, updater, tray, and host-metrics registration, grants, and migration contracts.
- Outbox 0016 requests a host-configuration locale contract and identifies the missing locale catalog/consumer decision.
- Outbox 0017 requested a diagnostics read-only call grant to `amaterasu.host-configuration/1`; that grant and the diagnostics dependency landed on main in `687f72a2` and `b084b0e7`.
- The coordinator confirmed LAN/WAN encryption fields share the one NVHTTP v16-to-v17 migration with frame-limiter and legacy preparation. Luna02 is the persistence writer; Luna06 will send `lan_encryption_mode` and `wan_encryption_mode` in the existing signed settings update and render their configured values without changing `/v1`.
- The confirmed frame-limiter contract uses decimal-Hz `frame_limiter_fps_limit` for persistence and scoped overrides. The management editor now preserves absent/null inheritance separately from false/zero and normalizes `rtss_disable_vsync_ullm` to `frame_limiter_disable_vsync`. The six scoped fields and global-only RTSS/MangoHUD fields are reflected in the editor allowlists.
- Next bounded slice: follow up on locale request 0016 with the coordinator for an approved host-configuration v1 contract and exact management/UI path ownership; do not extend host-configuration or write foreign-owned paths without that grant. The stream override editor is ready for L05 persistence only after coordinator/L05 confirms proposed catalog schema v2; L02/L05 still own runtime precedence and snapshots. The coordinator's clean integration checkout must run Rust checks, migration tests, browser qualification, and runtime/platform validation. Keep every unproven ID pending.


## Main validation: prerelease logging default

The diagnostics prerelease default candidate `a3476f12fefbd0774b54f8e136d8b931041dd653` landed on main as `c4ffb1ca27a13a55fbd799029d88a557e6838d06`. Coordinator validation passed:

- `cargo fmt --check -p amaterasu-diagnostics`.
- `cargo test -p amaterasu-diagnostics` — 24/24, including the three channel-default and explicit-override regressions.
- `cargo clippy -p amaterasu-diagnostics --all-targets -- -D warnings`.

Supported-platform runtime qualification remains pending. Full `global:min_log_level` parity also remains pending, including product registration/integration and end-to-end settings-to-consumer evidence. The lightweight lane did not run Cargo tests, builds, packaging, or Clippy. The NVENC editor remains gated on L02 confirmation of the exact v17 persisted/readback shape; no v18 is allocated.
