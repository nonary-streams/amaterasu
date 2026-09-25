# Luna-01 configuration parity handoff

## Lane and candidate

- Repository: `D:/sources/worktrees/amaterasu-config-foundation`
- Branch: `config-parity/foundation`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Lane starting/base commit: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`
- Latest code candidate: `44574af629eed41fb75cbc2a519e0b861c3a3cd6` (lane cherry-pick of package legacy-source discovery identity commit `eec8f2e65706d92f089c79613a7cdcbd1e41ed50`); this handoff/outbox update follows the code candidate.
- Reference: Vibeshine `vibe-test` commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`; used as behavioral evidence, not copied source.
- Session: `01a0d516-858e-7c40-b459-5de0bbf3d0dd`, `gpt-6-luna`, reasoning `max`.
- Lane status: all seven assigned IDs remain `pending`. The coordinator owns the shared coverage ledger and decides when evidence is sufficient to close an ID.

At startup I verified the repository root, branch, starting HEAD, source baseline, current model and reasoning effort, nested-worker availability, and inherited write permissions. The startup record is `outbox/luna-01/0001-startup.json`. The configured Nonary identity was used for the lane commits. Worker settings were `gpt-6-luna/xhigh`.

## Candidate commits

- `9e5500872d2437b59a31b70e6ae8d2b4b1509ec1` — classify and preserve configured legacy state inputs during package migration.
- `f863bbbe02d586740fa1ecc1fa2d282587c34c0b` — persist authentication sessions in a separate versioned sidecar and add revisioned TTL settings operations.
- `430cf52b4d27f5f7f15ce4aaa83d9360f0aef794` — add the v1 host-configuration provider and inject the trusted state root after package verification.
- `3902a21de4c3534cc3ac0c083219784b818753d6` — preserve configured aliases in migration snapshots, safely handle validated in-root links, and match Windows path casing with `CompareStringOrdinal`.
- `c8bed167242d475a21abc02c5d4e036b23ffe47d` — fix the coordinator's Windows sigma alias failure and `needless_return` finding; this is the lane cherry-pick of child commit `68e3d33df1b579ee39b72daa30aaa9b1e70da9ac`.
- `78fae895a3794f637c61639eddb5389ef50af0ef` — correct the Windows sigma regression test to expect rejection for a path that is not a Windows ordinal case variant; this is the lane cherry-pick of child commit `13272ef6ad22bbddb1e40b4776f7cf0892fd6448`.
- `e80ee741a86d2673bcef40c1cbc98dba936d9d08` — import the authentication session document version in its pipeline test; this is the lane cherry-pick of child commit `dccf6e3234d2a83d12ba4a0b34ca4b304c848a1d`.
- `b5c3d5f81a3ed23e5e184aa8106b2ff17747eac4` — register the v1 host-configuration provider in the Platform release profile, grant diagnostics its provider service call and management its update-capable provider call, and add profile tests; this is the lane cherry-pick of child commit `6c71ac4230f6bbc6f289b91324134fb519ca5b8f`.
- `b87dde93fa4a11cd08cd32ad4c01ea7c00b6988c` — declare diagnostics' initialization dependency on the host-configuration provider; this is the lane cherry-pick of child follow-up commit `0035a5bcc256219c0341f5c83e556bf889b02540`.
- `c68378d23b48bcfd41f8a2d32e34516ca5cb8cff` — correct authentication documentation for persisted session metadata and TTL behavior; this is the lane cherry-pick of child commit `4d96e11fee4ccbbe458dfa4489e7da23b8a59647`.

Each lane commit includes the required `Orchestrated By: GPT-6 Luna Max` and `Planned By: GPT-6 Astra High` lines.

## Coordinator validation follow-up

The coordinator ran `cargo test -p amaterasu-package` on the candidate in the clean source checkout: 43 tests passed, and the Windows-only `migration::tests::configured_identity_path_windows_ordinal_case_variants_share_one_snapshot_entry` failed for `CREDENTIALS/Σ.PEM` configured against the stored `credentials/ς.pem`. The operation surfaced as `configuration migration filesystem operation failed`. `cargo clippy -p amaterasu-package --all-targets -- -D warnings` also reported `clippy::needless_return` in the Windows branch of `is_reparse_or_symlink` at `migration.rs:1179`.

The failure occurred because `collect_configured_sensitive_files` called `canonicalize()` with the configured spelling before the existing Windows ordinal resolver could find the actual directory-entry spelling. The fix resolves each configured component through `resolved_existing_relative_path` first, then canonicalizes that actual spelling and keeps the canonical containment check. The cfg-specific Windows block now returns its boolean expression without `return`. The existing regression test is unchanged.

The isolated child worktree was `D:/sources/worktrees/amaterasu-config-package-alias-fix` on branch `config-parity/package-alias-fix`, based on lane HEAD `2ce42d5cf8d1b2f92c334c3104adba94f030e4fa`. CLI worker thread `01a0d569-2c09-7110-8fc5-97e583cf30d6` was verified from `state_5.sqlite` as `gpt-6-luna/xhigh`; its child commit is `68e3d33df1b579ee39b72daa30aaa9b1e70da9ac`. The lane lead reviewed the one-file diff and cherry-picked it to this lane as `c8bed167242d475a21abc02c5d4e036b23ffe47d`. Only `crates/package/src/migration.rs` changed in the correction.

After the cherry-pick, `rustfmt --edition 2024 --check crates/package/src/migration.rs crates/package/src/migration/tests.rs`, `git diff --check`, and `git show --check HEAD` passed in the lane. Tests and Clippy were not run here. The coordinator must rerun the Windows package test and Clippy on main; no post-fix execution or Windows qualification is claimed.

## Coordinator Windows casing test correction

The coordinator reran `cargo test -p amaterasu-package` after `c8bed167` and found only `configured_identity_path_windows_ordinal_case_variants_share_one_snapshot_entry` still failing. Direct Windows evidence confirmed that `CompareStringOrdinal("Σ.PEM", "ς.pem", ignoreCase=true)` returns not equal and that a stored `ς.pem` cannot be opened through the configured `Σ.PEM` spelling on this filesystem. The path resolver should therefore remain fail-closed; the regression test's assumption was incorrect.

The isolated child worktree was `D:/sources/worktrees/amaterasu-config-package-test-fix` on branch `config-parity/package-test-fix`, based on lane HEAD `ca38a57f3aa4f25435b9e46ed7789a55cd545d68`. CLI worker thread `01a0d57a-a2e1-7c63-bdbb-c4322bf9a054` was verified from `state_5.sqlite` as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/package/src/migration/tests.rs`, renamed the case to `configured_identity_path_windows_sigma_mismatch_is_rejected`, and asserts collection rejects the mismatched configured spelling. The separate positive ASCII and É/é case-insensitive tests were preserved. The lane lead reviewed the one-file diff and cherry-picked child commit `13272ef6ad22bbddb1e40b4776f7cf0892fd6448` to this lane as `78fae895a3794f637c61639eddb5389ef50af0ef`.

`rustfmt --edition 2024 --check crates/package/src/migration.rs crates/package/src/migration/tests.rs` and `git diff --check` passed in both lightweight worktrees; the lane checked its committed test correction with `git diff --check HEAD^ HEAD`. No tests, builds, or Clippy were run. The coordinator must rerun `cargo test -p amaterasu-package` and strict package Clippy on main to confirm the corrected expectation and prior implementation findings.

## Coordinator authentication test compile correction

The coordinator cherry-picked the authentication session/TTL candidate `f863bbbe` to main as `e805ca54`. Its `cargo fmt --check --package amaterasu-authentication` passed, but `cargo test -p amaterasu-authentication` failed to compile the test at `modules/authentication/src/pipeline/tests.rs:721` because `SESSION_DOCUMENT_VERSION` was not imported; rustc suggested `crate::protocol::SESSION_DOCUMENT_VERSION`.

The isolated child worktree was `D:/sources/worktrees/amaterasu-config-auth-test-import-fix` on branch `config-parity/auth-test-import-fix`, based on lane HEAD `d6eee5c05290633fea656219b34dc3c6c5eb6a0c`. CLI worker thread `01a0d58d-825f-7922-b732-071a0e697a03` was verified from `state_5.sqlite` as `gpt-6-luna/xhigh` before edits were authorized. It changed only `modules/authentication/src/pipeline/tests.rs`, adding `use crate::protocol::SESSION_DOCUMENT_VERSION;`; behavior and test assertions are unchanged. The lane lead reviewed the one-line diff and cherry-picked child commit `dccf6e3234d2a83d12ba4a0b34ca4b304c848a1d` to this lane as `e80ee741a86d2673bcef40c1cbc98dba936d9d08`.

`rustfmt --edition 2024 --check modules/authentication/src/pipeline/tests.rs` and `git diff --check` passed in the child and lane; no Cargo tests, builds, or Clippy were run there. The coordinator later confirmed the authentication candidate on main: `cargo fmt --check -p amaterasu-authentication`, `cargo test -p amaterasu-authentication` (37/37), and strict Clippy passed in main commits `ce6a4330` and `65c1110c`. Luna-01 did not edit main.

## Coordinator authentication fixture test correction

The coordinator reported two authentication test failures after the missing session-version import was fixed. Source tracing confirmed both failures came from stale test setup, with no production behavior defect. The setup pipeline reads the main authentication document and session sidecar in separate asynchronous ticks; the persistence test had advanced only through the first load before expecting a submitted write. The legacy password test injected a document into the harness with revision zero, and `read_document()` correctly rejects a found document with revision zero as `invalid_state` before verifying the password.

The isolated child worktree was `D:/sources/worktrees/amaterasu-config-auth-fixture-fix`, branch `config-parity/auth-fixture-fix`, based on lane HEAD `bcdd37605c6593a67f1edb80cef2fd975e01aba4`. CLI worker thread `01a0d59f-0363-7db0-906f-1ee42a2eade9` was verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `modules/authentication/src/pipeline/tests.rs`: the persistence test now advances through the session document load before checking for the write, and the legacy fixture uses revision 1. The lead reviewed the one-file diff and cherry-picked child commit `0cbf5aaf40ccf19ec38f648257614da091ddb9d6` into this lane as `f0a2f23c19305348489061590bb35bb365451181`.

`rustfmt --edition 2024 --check modules/authentication/src/pipeline/tests.rs` and `git diff --check HEAD^ HEAD` passed in the lane; both checks also passed in the child. No Cargo tests, build, or Clippy were run. The next separate correction replaced the two `unnecessary_map_or` calls. The coordinator's later main validation passed authentication formatting, 37/37 tests, and strict Clippy as recorded above. Luna-01 did not edit main.

## Coordinator authentication test Clippy correction

The coordinator's strict authentication Clippy run found two `clippy::unnecessary_map_or` expressions in `modules/authentication/src/pipeline/tests.rs`. They are still present after the fixture correction and both encode the same absent-or-empty session assertion. The isolated child changed only those two calls to `is_none_or`, preserving the test behavior.

The child worktree was `D:/sources/worktrees/amaterasu-config-auth-clippy-fix`, branch `config-parity/auth-clippy-fix`, based on lane HEAD `50bee00f4d7672cdc1ba3b4a281e5df5c78a05ee`. CLI worker thread `01a0d5a4-46fe-7972-a390-b602536f3024` was verified from session state as `gpt-6-luna/xhigh` before edits were authorized. The lead reviewed the two-line diff and cherry-picked child commit `23f301d941c45d2261b0a0753f85d464626fce8a` into the lane as `f3b78b7b77db7f94ecf7d0671c0df04d4f44e8cf`.

`rustfmt --edition 2024 --check modules/authentication/src/pipeline/tests.rs` and `git diff --check` passed in the child; rustfmt and `git diff --check HEAD^ HEAD` passed in the lane. No Cargo tests, build, or Clippy were run there. The coordinator later confirmed authentication formatting, 37/37 tests, and strict Clippy passed on main in commits `ce6a4330` and `65c1110c`. Luna-01 did not edit main.

## Coordinator host-provider strict Clippy correction

The coordinator reported three strict-Clippy findings in the host provider: a needless `return` in the Windows reparse-point check, a default-initialized `Document` followed by `log_path` assignment, and an explicit loop counter in the management update test. The isolated child kept the Windows file-attribute predicate unchanged as the block expression, initialized test documents with struct update syntax, and changed the fixed three-iteration loop to `1..=3`; the read/write response and revision assertions are unchanged. It also converted the same `Document::default()` field-reassignment pattern in the Unix-only symlink test so that Unix Clippy does not retain the equivalent lint.

The child worktree was `D:/sources/worktrees/amaterasu-config-host-provider-clippy-fix`, branch `config-parity/host-provider-clippy-fix`, based on lane HEAD `5d23435d933d04a6f99baf9a89bbf3207b37887a`. CLI worker thread `01a0d5a7-3074-7cc3-8cc2-575d058bd81c` was verified from session state as `gpt-6-luna/xhigh` before edits were authorized. The lead reviewed the two-file diff and cherry-picked child commit `0c10dcb42e28eaeaf0614afd9df6ac0b6812bc50` into the lane as `1c1d99cc21682e4c2594f1fbf9d231cd5f172470`.

`rustfmt --edition 2024 --check modules/host-configuration/src/path.rs modules/host-configuration/src/pipeline/tests.rs` and `git diff --check` passed in the child; rustfmt and `git diff --check HEAD^ HEAD` passed in the lane. No Cargo tests, build, or Clippy were run. The coordinator later confirmed `cargo fmt --check`, host-configuration tests (15/15), host tests (17/17), and strict Clippy for both packages passed on main commit `45aa8882`. Windows reparse-point and Unix symlink runtime qualification remain unrun. Luna-01 did not edit main.

## Coordinator main landing confirmations

The coordinator cherry-picked the authentication fixture correction `f0a2f23c19305348489061590bb35bb365451181` and authentication test Clippy correction `f3b78b7b77db7f94ecf7d0671c0df04d4f44e8cf` to main as `ce6a4330` and `65c1110c`; authentication format, 37/37 tests, and strict Clippy passed. It cherry-picked host-provider Clippy correction `1c1d99cc21682e4c2594f1fbf9d231cd5f172470` as `45aa8882`; host-configuration format, 15/15 tests, host tests 17/17, and strict Clippy passed. At that checkpoint Windows reparse-point and Unix symlink qualification, package profile registration, startup readiness, diagnostics consumption, and management editor integration were pending. The later profile assignment and readiness decision are recorded below; main validation and platform qualification remain pending.

## Host state-root serialization correction

Host startup previously converted its canonical state root with `to_string_lossy()` when passing it through `RuntimeConfig.host_context`, the optional host-configuration component setting, and diagnostics bootstrap logging. On a native path with invalid Unicode, replacement characters could make a different path appear in the JSON context. `crates/host` now converts the checked path with `Path::to_str()` and fails closed if it cannot be represented. The validation rejects invalid input or canonical paths before the Windows case-insensitive string comparison; the same exact canonical string feeds both runtime fields. Logging configuration now returns an error for an unrepresentable root, and the worker propagates it before startup.

The isolated child worktree was `D:/sources/worktrees/amaterasu-config-host-state-root-serialization`, branch `config-parity/host-state-root-serialization`, based on lane HEAD `ac7fb5b716b9a75333e903c780ad31a203968fcc`. CLI worker thread `01a0d5b6-b822-7bf0-bca5-934a707a8c75` was verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/host/src/loading.rs`, `crates/host/src/logging_config.rs`, and `crates/host/src/worker.rs`, adding cfg-specific tests for non-Unicode Unix bytes and an unpaired Windows UTF-16 surrogate. The lead reviewed the three-file diff and cherry-picked child commit `e8e8ae70ae098f3fb808228ce3c19abf282843f6` into the lane as `efd7113323dc93676ccf9d6318800c722f89d130`.

`rustfmt --edition 2024 --check crates/host/src/loading.rs crates/host/src/logging_config.rs crates/host/src/worker.rs` and `git diff --check` passed in the child and lane. Cargo tests, build, Clippy, and both platform runtime tests were not run in this lightweight lane. The coordinator should run host tests on Windows and Unix to exercise the new native-path cases. The profile assignment and READY semantics questions in outbox `0023-host-startup-registration-readiness-dependencies.json` have been answered; frame-limiter identity and wire/lifecycle acknowledgments remain unresolved.

## Exact changed paths

- `crates/package/src/migration.rs`
- `crates/package/src/migration/tests.rs`
- `modules/authentication/src/engine.rs`
- `modules/authentication/src/migration.rs`
- `modules/authentication/src/pipeline.rs`
- `modules/authentication/src/pipeline/tests.rs`
- `modules/authentication/src/protocol.rs`
- `modules/host-configuration/Cargo.toml`
- `modules/host-configuration/README.md`
- `modules/host-configuration/src/lib.rs`
- `modules/host-configuration/src/path.rs`
- `modules/host-configuration/src/pipeline.rs`
- `modules/host-configuration/src/pipeline/tests.rs`
- `modules/host-configuration/src/protocol.rs`
- `modules/host-configuration/src/settings.rs`
- `crates/host/src/loading.rs`
- `crates/host/src/logging_config.rs`
- `crates/host/src/worker.rs`
- `crates/release/src/profiles.rs`
- `crates/release/src/profiles/tests.rs`
- `Cargo.toml`
- `Cargo.lock`

## Implemented behavior and remaining parity

| Stable ID | Current evidence | Remaining work; keep pending |
| --- | --- | --- |
| `global:file_state` | Host provider stores and validates the path; default is `sunshine_state.json`. Package migration classifies configured contained state files, including sensitive-state gates, settings, and applications data. This path remains a legacy import input and does not redirect Amaterasu state storage. | Signed management editor and caller authorization, observable legacy importer use, complete migration behavior, and Windows/Unix qualification. |
| `global:vibeshine_file_state` | Host provider stores and validates the auxiliary legacy import path; default is `vibeshine_state.json`. Package migration recognizes configured contained targets. | Signed management editor and caller authorization, observable Vibeshine importer use, complete migration behavior, and Windows/Unix qualification. |
| `global:credentials_file` | Host provider treats an omitted or empty value as inheriting `file_state`; explicit paths override it. Package migration preserves configured contained identity/credential files under their configured archive names. | Luna-02 authentication/identity consumers, signed management editor and authorization, pairing parser comment compatibility, and platform qualification. |
| `global:pkey` | Host provider validates the private-key path; default is `credentials/cakey.pem`. Package migration snapshots configured contained key files under the path referenced by the imported configuration. | Luna-02 pairing/TLS consumer integration, signed management editor and authorization, secret handling review, and platform qualification. |
| `global:cert` | Host provider validates the certificate path; default is `credentials/cacert.pem`. Package migration snapshots configured contained certificate files under the configured archive name. | Luna-02 pairing/TLS consumer integration, signed management editor and authorization, and platform qualification. |
| `global:session_token_ttl_seconds` | Authentication default is 7200 seconds; allowed values are `1..=i32::MAX`. Settings use a revisioned `amaterasu.configuration/1` operation. Session state is stored in a separate strict `authentication/sessions/v1` sidecar, capped at 16 records and 12,000 bytes. Credential and scope digests are checked; absolute expiry is retained; newly issued tokens wait for durable state acknowledgement. | Luna-06 signed route/editor, browser reload and cookie behavior, restart evidence, and integrated qualification. Vibeshine web settings have an unset-value default of 86400 seconds, which conflicts with the host default and needs an explicit integration decision. |
| `global:remember_me_refresh_token_ttl_seconds` | Authentication default is 604800 seconds; allowed values are `1..=i32::MAX`. Refresh sessions persist absolute expiry and rotation in the sidecar; TTL edits do not rewrite existing session expiries. | Luna-06 signed route/editor and refresh-cookie behavior, browser/restart evidence, and integrated qualification. |

Authentication management operations are `settings_get` and `settings_update`; updates carry an expected revision and return a revision conflict for stale writes. HTTP cookie issuance and validation remain in Luna-06. The coordinator specified access cookie `Path=/v1/` and refresh cookie `Path=/v1/auth`; both must remain host-only, `Secure`, `HttpOnly`, and `SameSite=Strict`, with management loopback-only and existing Origin/CORS checks.

The package migration now strips inline `#` comments for configured identity paths, preserves the configuration's alias in the archive while copying from the canonical contained target, and accepts a link/reparse entry only when a validated configured alias covers that exact resolved entry. Unconfigured links still fail. Windows directory-entry matching uses `CompareStringOrdinal`; ambiguous case-insensitive matches fail closed. Unix symlink tests and positive Windows ASCII and É/é casing tests are present, along with a Windows sigma-mismatch rejection regression. None have been executed here.

## Provider contract and integration dependencies

The provider is `amaterasu.host-configuration/1`, with revisioned document version 1 at state key `global/amaterasu.host-configuration`. Its fields are `file_state`, `vibeshine_file_state`, `credentials_file`, `pkey`, `cert`, `log_path`, and `min_log_level`. Defaults are `sunshine_state.json`, `vibeshine_state.json`, empty/inherited credentials path, `credentials/cakey.pem`, `credentials/cacert.pem`, `amaterasu.log`, and `product_default` respectively. Product default logging resolves to `info` for stable and `debug` for prerelease builds. Only the management component may update the document; consumers receive validated read-only effective values. Readiness becomes true after an authoritative state read and document validation. Relative paths resolve under the canonical host-injected state root; explicit absolute paths remain absolute. `credentials_file` inherits `file_state` when empty.

`crates/host/src/loading.rs` injects `host_state_root` after package verification and overwrites any package-supplied value. Luna-06 handoff `0018-diagnostics-host-logging-slice-handoff.json` records an asynchronous reader for `amaterasu.host-configuration/1`: diagnostics keeps validated signed/bootstrap logging while the provider is unavailable, invalid, or at revision zero, and adopts only a ready persisted revision above zero. The coordinator confirmed worker `READY` is independent of this hydration; no startup deadline or log buffer is required, and the 30-second refresh may continue after `READY`.

Dependencies and blockers:

- Luna-02 owns `modules/pairing/src/migration.rs` and the identity/legacy path consumers. Request `0010-pairing-config-comment-dependency.json` asks it to strip inline `#` comments consistently with the auth parser and add integration coverage. No response is in the inbox.
- Luna-06 owns the signed settings editor/routes, HTTP cookies, and `modules/diagnostics/**`. The auth settings and provider contracts were sent in `0011-auth-ttl-management-consumer-contract.json` and `0004-host-configuration-contract.json`; cookie paths were confirmed by the coordinator. Logger consumer design is pending.
- The coordinator assigned `crates/release/src/profiles.rs` and `crates/release/src/profiles/tests.rs` to L01. Commits `b5c3d5f81a3ed23e5e184aa8106b2ff17747eac4` and `b87dde93fa4a11cd08cd32ad4c01ea7c00b6988c` register the provider in Platform, include diagnostics and management service grants, and order diagnostics after provider initialization. Coordinator validation passed on main: `cargo test -p amaterasu-release` (20/20) and `cargo clippy -p amaterasu-release --all-targets -- -D warnings`. Signed-package lifecycle and platform qualification remain outstanding.
- Frame-limiter registration remains blocked on L03's committed service component/artifact identity and the L02/L05/L06 wire/lifecycle acknowledgments. L03's cited `5790c4ab` is documentation-only and does not establish a component or artifact identity. No frame-limiter component or caller grant was added.
- The shared parity documents, coverage ledger, and integration ledger are coordinator-owned. This handoff is the lane-owned record; no shared ledger was modified.

## Coordinator host integration and dependency follow-up

The coordinator confirmed the Luna-06 cookie contract: the access cookie uses `Path=/v1/`, the refresh cookie uses `Path=/v1/auth`, and both remain host-only, `Secure`, `HttpOnly`, and `SameSite=Strict`. HTTP issuance, validation, and frontend behavior remain Luna-06-owned.

The coordinator assigned Luna-01 the host/package side of `amaterasu.frame-limiter/1` registration, while Luna-03 exclusively owns `modules/frame-limiter/**`. L01 must wait for L03's committed service component/artifact identity and L02/L05/L06 wire/lifecycle acknowledgments before adding any frame-limiter component or caller grant. The existing generic host path verifies and loads ordered package components, then ticks and health-checks every loaded module; it has no frame-limiter-specific registration logic. No frame-limiter registration was added in this slice.

The coordinator extended `amaterasu.host-configuration/1` and revisioned document v1 to include typed global `log_path` and `min_log_level`. The provider implementation already matches the extension: defaults are `amaterasu.log` and `product_default`; the typed levels are `product_default`, `verbose`, `debug`, `info`, `warning`, `error`, `fatal`, and `none`, with product defaults resolving to stable `info` and prerelease `debug`. `get` and `readiness` are read-only; readiness requires authoritative state read and v1/path validation. `update` is restricted to immediate caller `amaterasu.management`, replaces the full document using `expected_revision`, and acknowledges only after durable state CAS. Luna-06 management routes should read the current revision and send a full replacement; after `revision_conflict`, they should reread before retrying. The concrete contract and remaining logger integration decision are in outbox `0018-host-global-contract-ack.json`.

The coordinator's readiness decision is that host worker `READY`/`service-ready.json` does not wait for diagnostics to hydrate persisted logging. Diagnostics starts with validated signed/bootstrap `host_logging`, retains that policy while the provider is unavailable or reports revision zero, and adopts only a ready persisted revision. No startup deadline or buffer is needed; refresh may continue every 30 seconds after `READY`. L06's diagnostics changes are recorded in its handoff `0018-diagnostics-host-logging-slice-handoff.json`; no diagnostics-owned path was edited by L01.

## Coordinator release-profile assignment and implementation

Inbox requests `coordinator-release-profile-ownership-and-readiness-20260924-01` and `coordinator-host-path-and-profile-followup-20260924-01` assign the exact paths `crates/release/src/profiles.rs` and `crates/release/src/profiles/tests.rs` to L01. The `Platform` profile now registers component `amaterasu.host-configuration`, artifact `amaterasu_host_configuration`, and service `amaterasu.host-configuration/1`, with runtime and `amaterasu.state` dependencies. The provider calls `amaterasu.state/1`; diagnostics has only the host-configuration service call and depends on that provider component so package initialization orders the provider first; management has the service call and provider dependency. The service-grant model is component-level, while the provider authorizes `Update` only from caller `amaterasu.management`. Luna-06's consumer reads with `get`; it receives no direct state grant. No provider schema or persistence version changed.

The isolated worker worktree was `D:/sources/worktrees/amaterasu-host-profile-registration`, branch `config-parity/host-profile-registration`, based on `e101ca98c10fdcba54fa339da35e165523bb16e1`. CLI thread `01a0d5c5-480b-7461-8ce0-a567c00e1153` was verified from session state as `gpt-6-luna/xhigh` before edits and again before its follow-up. The worker committed only the two assigned files: provider/grant implementation `6c71ac4230f6bbc6f289b91324134fb519ca5b8f` and dependency-order follow-up `0035a5bcc256219c0341f5c83e556bf889b02540` (parent `6c71ac4230f6bbc6f289b91324134fb519ca5b8f`). The lane lead reviewed both diffs and the profile tests, then cherry-picked them as `b5c3d5f81a3ed23e5e184aa8106b2ff17747eac4` and `b87dde93fa4a11cd08cd32ad4c01ea7c00b6988c`. Edition-2024 rustfmt check and `git diff --check` passed in child and lane. Cargo tests/builds/Clippy were not run in either lightweight worktree; coordinator main validation later passed profile tests 20/20 and strict release-profile Clippy. Signed-package lifecycle and platform qualification remain outstanding.

Frame-limiter remains explicitly unregistered until L03 supplies a committed service component and artifact identity and L02/L05/L06 acknowledge its wire/lifecycle contract. The docs-only `5790c4ab` reference does not resolve component identity. The host-configuration profile's coordinator Cargo validation has passed; runtime qualification remains pending. Outbox `0026-host-configuration-profile-registration-followup.json` recorded that profile candidate, and outbox `0027-authentication-session-persistence-documentation.json` records the next lane update.

## Workers, reviews, checks, and qualification

- `state_paths_worker` (`gpt-6-luna/xhigh`) implemented the package migration slices in the two package paths above. `state_paths_reviewer` (`gpt-6-luna/xhigh`) reviewed the source changes, including the Windows FFI and sigma case; the coordinator's package execution later found the pre-canonicalization lookup failure documented above.
- The isolated package correction worker used CLI thread `01a0d569-2c09-7110-8fc5-97e583cf30d6`; its actual model and effort were verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/package/src/migration.rs` and created child commit `68e3d33df1b579ee39b72daa30aaa9b1e70da9ac`.
- The isolated Windows sigma test correction worker used CLI thread `01a0d57a-a2e1-7c63-bdbb-c4322bf9a054`; its actual model and effort were verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/package/src/migration/tests.rs` and created child commit `13272ef6ad22bbddb1e40b4776f7cf0892fd6448`.
- The isolated authentication test import worker used CLI thread `01a0d58d-825f-7922-b732-071a0e697a03`; its actual model and effort were verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `modules/authentication/src/pipeline/tests.rs` and created child commit `dccf6e3234d2a83d12ba4a0b34ca4b304c848a1d`.
- `ttl_worker` (`gpt-6-luna/xhigh`) implemented the authentication sidecar and settings operations in the five authentication paths. An independent review found no actionable findings.
- `host_config_worker` (`gpt-6-luna/xhigh`) implemented the provider in `modules/host-configuration/**` and performed a read-only host startup/logging investigation. A separate review found no actionable findings in the provider and trusted-root injection.
- Package, authentication, provider, host-loading, and release-profile files passed `rustfmt --edition 2024 --check`; `git diff --check` passed. `cargo metadata --locked --offline --no-deps --format-version 1` passed after adding the provider crate, before profile registration.
- No Rust tests, builds, packaging, host lifecycle checks, or Clippy were run, as required for this lightweight worktree. No Unix or Windows runtime qualification is claimed.

In the reserved clean source checkout, run `cargo fmt --check`, `cargo test -p amaterasu-package`, `cargo test -p amaterasu-authentication`, `cargo test -p amaterasu-host-configuration`, host signed-package lifecycle/readiness and grant tests, and strict workspace Clippy (for example, `cargo clippy --workspace --all-targets -- -D warnings`). Run migration symlink tests on Unix and the Windows casing/reparse tests on Windows. Validate real signed package startup after registration is resolved. The coordinator owns that integration checkout and final qualification.

## Next bounded slice

The authentication session persistence/TTL documentation correction is committed, and the host-configuration release profile has passed coordinator tests and strict Clippy on main. Continue L01-owned host path and package state-import work where the existing code establishes a self-contained behavior; coordinate before depending on L02 pairing/authentication consumers or L06 management/browser consumers. Do not change release profiles without a new provider identity/dependency. Keep frame-limiter pending until L03 provides a committed component/artifact identity and L02/L05/L06 acknowledge the wire/lifecycle contract. Keep all seven assigned IDs `pending` until consumer integration, signed editing, runtime behavior, and platform qualification are reviewed.

## Authentication session persistence and TTL documentation correction

The authentication README previously said active sessions and refresh-token digests existed only in memory and were lost on module restart. That no longer matched the backend's separate versioned `authentication/sessions/v1` state document. It now records that the document stores bounded session metadata and digests rather than raw token secrets, eligible records restore only when credential/scope digests still match and the absolute refresh expiry has not passed, and session state writes require durable state acknowledgement. The TTL text now matches the engine: login chooses the remembered configured refresh TTL or fixed 86,400-second unremembered TTL, then uses the larger of that and the access TTL; TTL edits and refresh rotation do not move the session's absolute refresh expiry.

Stable IDs: `global:session_token_ttl_seconds` and `global:remember_me_refresh_token_ttl_seconds`. Exact changed path: `modules/authentication/README.md`.

The isolated worker worktree was `D:/sources/worktrees/amaterasu-auth-session-doc-fix`, branch `config-parity/auth-session-doc-fix`, based on lane HEAD `c29851303c50c091e9b9b1acc82d7c12dda0a83c`. CLI thread `01a0d5e4-7ea1-7951-8c3a-32f89cf47f06` was verified from session state as `gpt-6-luna/xhigh` before edits and again after commit. It changed only `modules/authentication/README.md` and committed `4d96e11fee4ccbbe458dfa4489e7da23b8a59647`; the lane lead reviewed the one-file diff and cherry-picked it as `c68378d23b48bcfd41f8a2d32e34516ca5cb8cff`.

`rustfmt --edition 2024 --check modules/authentication/src/engine.rs modules/authentication/src/protocol.rs` and `git diff --check` passed in the child. The lane also passed `rustfmt --edition 2024 --check modules/authentication/src/engine.rs modules/authentication/src/protocol.rs modules/authentication/src/pipeline.rs`, `git diff --check HEAD^ HEAD`, and `git show --check HEAD`. No Cargo tests, builds, or Clippy were run for this documentation-only slice; coordinator main validation of the existing authentication code was previously reported as formatting passed, 37/37 tests, and strict Clippy passed. This slice adds no platform evidence.

The session persistence and TTL implementation remains subject to integrated L06 signed settings routes, browser/cookie behavior, and restart qualification. The legacy web default of 86,400 seconds for session token TTL still conflicts with the host default of 7,200 seconds and requires an explicit integration decision. Those gaps remain pending; no shared coverage ledger was edited.

## Package legacy-source discovery identity correction

The package discovery path previously deduplicated predecessor roots by lowercasing `to_string_lossy()` output. That could merge distinct case-sensitive or non-Unicode roots on Unix and did not normalize filesystem aliases such as `nested/../Sunshine`.

The isolated worker verified from session state as `gpt-6-luna/xhigh` was thread `01a0d608-9801-7762-925a-3f5f60fe476a` (`GPT 6-Luna X-High`). It worked in `D:/sources/worktrees/amaterasu-package-discovery-identity` on branch `config-parity/package-discovery-identity`, based on lane HEAD `763a02a682f08f99ea733cb503af90a3107d4639`. It changed only `crates/package/src/migration.rs` and `crates/package/src/migration/tests.rs`, validating existing roots before canonicalization, preserving exact canonical path identity on Unix, and resolving Windows paths with the existing `CompareStringOrdinal` component matcher. Regression cases cover lexical aliases, case-distinct and non-Unicode Unix roots, and Windows ordinal case variants.

The child commit is `eec8f2e65706d92f089c79613a7cdcbd1e41ed50`. The lane lead reviewed its two-file diff and cherry-picked it to `config-parity/foundation` as `44574af629eed41fb75cbc2a519e0b861c3a3cd6`. `rustfmt --edition 2024 --check crates/package/src/migration.rs crates/package/src/migration/tests.rs`, `git diff --check`, and `git show --check` passed in the child and lane. No Cargo tests, builds, Clippy, Unix execution, or Windows execution were run in the lightweight worktrees. The coordinator should run `cargo test -p amaterasu-package` and `cargo clippy -p amaterasu-package --all-targets -- -D warnings` on main, and execute the platform-specific discovery cases on Unix and Windows. The `global:file_state` and `global:vibeshine_file_state` IDs remain pending for observable importer use, signed management editing, complete migration behavior, and full platform qualification.

## Next bounded slice

Continue a self-contained L01-owned host or package migration gap. Keep authentication consumer, pairing/TLS, and management editor integration coordinated with L02/L06, preserve all seven assigned IDs as pending, and leave frame-limiter profile registration blocked on its cross-lane contract.
