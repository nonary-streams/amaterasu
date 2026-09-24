# Luna-01 configuration parity handoff

## Lane and candidate

- Repository: `D:/sources/worktrees/amaterasu-config-foundation`
- Branch: `config-parity/foundation`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Lane starting/base commit: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`
- Latest implementation candidate: `78fae895a3794f637c61639eddb5389ef50af0ef` (lane cherry-pick of test correction child commit `13272ef6ad22bbddb1e40b4776f7cf0892fd6448`)
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

`crates/host/src/loading.rs` injects `host_state_root` after package verification and overwrites any package-supplied value. Host logging still reads bootstrap policy. The runtime settings snapshot is immutable after load, and diagnostics currently opens its logger synchronously from bootstrap injection. A provider-backed logger needs Luna-06 diagnostics to asynchronously load the provider, hold log records until settings are ready, and coordinate readiness if `READY` is expected to guarantee logger initialization. The implementation design and readiness questions were sent in `outbox/luna-01/0009-host-logging-bootstrap-contract.json` and `0012-persisted-logger-consumer-design.json`; no decision is in the inbox yet.

Dependencies and blockers:

- Luna-02 owns `modules/pairing/src/migration.rs` and the identity/legacy path consumers. Request `0010-pairing-config-comment-dependency.json` asks it to strip inline `#` comments consistently with the auth parser and add integration coverage. No response is in the inbox.
- Luna-06 owns the signed settings editor/routes, HTTP cookies, and `modules/diagnostics/**`. The auth settings and provider contracts were sent in `0011-auth-ttl-management-consumer-contract.json` and `0004-host-configuration-contract.json`; cookie paths were confirmed by the coordinator. Logger consumer design is pending.
- Signed module/profile registration still needs `crates/release/src/profiles.rs` and `crates/release/src/profiles/tests.rs`, which are not owned by this lane. Exact-path ownership was requested in `0008-release-profile-registration-ownership.json`; no answer is in the inbox. The provider cannot be fully exercised in a signed package until profile registration and grants are resolved.
- The coordinator assigned Luna-01 the host/package registration path for `amaterasu.frame-limiter/1`; Luna-03 exclusively owns `modules/frame-limiter/**` and its service manifest/ABI contract. Registration awaits Luna-03's contract and must not edit that module path.
- The shared parity documents, coverage ledger, and integration ledger are coordinator-owned. This handoff is the lane-owned record; no shared ledger was modified.

## Workers, reviews, checks, and qualification

- `state_paths_worker` (`gpt-6-luna/xhigh`) implemented the package migration slices in the two package paths above. `state_paths_reviewer` (`gpt-6-luna/xhigh`) reviewed the source changes, including the Windows FFI and sigma case; the coordinator's package execution later found the pre-canonicalization lookup failure documented above.
- The isolated package correction worker used CLI thread `01a0d569-2c09-7110-8fc5-97e583cf30d6`; its actual model and effort were verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/package/src/migration.rs` and created child commit `68e3d33df1b579ee39b72daa30aaa9b1e70da9ac`.
- The isolated Windows sigma test correction worker used CLI thread `01a0d57a-a2e1-7c63-bdbb-c4322bf9a054`; its actual model and effort were verified from session state as `gpt-6-luna/xhigh` before edits were authorized. It changed only `crates/package/src/migration/tests.rs` and created child commit `13272ef6ad22bbddb1e40b4776f7cf0892fd6448`.
- `ttl_worker` (`gpt-6-luna/xhigh`) implemented the authentication sidecar and settings operations in the five authentication paths. An independent review found no actionable findings.
- `host_config_worker` (`gpt-6-luna/xhigh`) implemented the provider in `modules/host-configuration/**` and performed a read-only host startup/logging investigation. A separate review found no actionable findings in the provider and trusted-root injection.
- Package, authentication, provider, and host-loading files passed `rustfmt --edition 2024 --check`; `git diff --check` passed. `cargo metadata --locked --offline --no-deps --format-version 1` passed after adding the provider crate.
- No Rust tests, builds, packaging, host lifecycle checks, or Clippy were run, as required for this lightweight worktree. No Unix or Windows runtime qualification is claimed.

In the reserved clean source checkout, run `cargo fmt --check`, `cargo test -p amaterasu-package`, `cargo test -p amaterasu-authentication`, `cargo test -p amaterasu-host-configuration`, host signed-package lifecycle/readiness and grant tests, and strict workspace Clippy (for example, `cargo clippy --workspace --all-targets -- -D warnings`). Run migration symlink tests on Unix and the Windows casing/reparse tests on Windows. Validate real signed package startup after registration is resolved. The coordinator owns that integration checkout and final qualification.

## Next bounded slice

Resolve ownership of the two `crates/release` profile files and the diagnostics startup/readiness contract. Receive Luna-02's parser/consumer response and Luna-03's frame-limiter service contract. Then implement only the approved host/package registration and diagnostics-independent host work within the assigned paths, update the dependency messages, and submit the next code candidate with all seven IDs still `pending` until cross-lane consumers, signed editing, runtime behavior, and platform evidence are reviewed.
