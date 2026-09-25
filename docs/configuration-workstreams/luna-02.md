# Luna-02 network configuration parity handoff

## Candidate

- Branch: `config-parity/network`
- Base SHA: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`
- Candidate code SHA: `db14b28e5b0d1da50382f02b02052c4ebc67feb4`
- Source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`
- Behavioral reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`
- CLI session: `01a0d516-8616-7270-a509-547be018f212` (`gpt-6-luna`, `max`)

## Bounded slice: scoped stream controls

Stable IDs touched: `global:max_bitrate`, `global:minimum_fps_target`, and
`global:video_max_batch_size_kb`. The runtime launch projection now resolves
sparse client values before application values before the global launch
snapshot. Client values are certificate-bound through pairing state. Explicit
zero remains distinct from absence: zero/negative bitrate disables the cap,
zero minimum FPS uses negotiated FPS, and zero or unsupported in-range batch
sizes use the existing 64 KiB runtime fallback.

RTSP receives the bitrate cap separately from the configured profile. Its
effective bitrate now governs encoder startup, ANNOUNCE matching, and reported
capabilities, so a capped launch remains negotiable. Older RTSP Open payloads
without the optional field keep their configured bitrate. Pairing records load
without the newly added keys as unset.

Changed owned paths:

- `modules/nvhttp/src/launch.rs`
- `modules/nvhttp/src/launch/record.rs`
- `modules/nvhttp/src/launch/tests.rs`
- `modules/nvhttp/src/launch/workflow.rs`
- `modules/nvhttp/src/wire.rs`
- `modules/pairing/src/pipeline/tests.rs`
- `modules/pairing/src/protocol.rs`
- `modules/rtsp/src/controller/network.rs`
- `modules/rtsp/src/controller/record.rs`
- `modules/rtsp/src/model.rs`

## Review and validation

- Pairing override candidate received a separate read-only review. The reviewer
  found no correctness or compatibility blocker and suggested checking that an
  invalid update does not persist; that rejection-path test is now included.
- Lead review found and fixed an RTSP negotiation gap: the capped encoder rate
  must also be used for ANNOUNCE matching and capabilities.
- `rustfmt --edition 2024 --check` passed for all ten changed Rust files.
- `git diff --check` passed.
- Rust tests, builds, and Clippy were not run. This lightweight worktree is
  restricted from those checks; integration validation should run:

  ```powershell
  cargo test -p amaterasu-nvhttp
  cargo test -p amaterasu-pairing
  cargo test -p amaterasu-rtsp
  cargo clippy -p amaterasu-nvhttp -p amaterasu-pairing -p amaterasu-rtsp --all-targets
  ```

- No platform qualification was run. In particular, the capped encoder path
  still needs Windows integration evidence from the reserved clean source
  checkout.

## Worker assignments

- `network_policy_scan` (`gpt-6-luna`, `xhigh`): added sparse client bitrate,
  minimum-FPS, and video-batch fields and validation in pairing records, with
  persistence, legacy-document, zero/absence, and range coverage.
- `stream_policy_scan` (`gpt-6-luna`, `xhigh`): added the optional RTSP Open
  bitrate ceiling and encoder-start behavior with old-payload and boundary
  coverage.
- `review_pairing_overrides`: read-only review of the pairing candidate. Its
  effective model metadata was not available through the agent status tools.

The lead integrated and reviewed both implementation slices, updated NVHTTP
launch resolution and RTSP negotiation/capability consumers, and owns the
resulting lane commits.

## Remaining mismatches and coordination

- All 18 assigned IDs remain `pending` in the coordinator-owned coverage
  ledger. This slice is partial evidence for the three stream IDs; it does not
  establish full settings-editor, persistence, authorization, restart, or
  platform parity.
- Per-application catalog/launch persistence and per-client management/editor
  support remain owned by luna-05/luna-06. Request
  `luna-02-stream-overrides-01` is still awaiting their acknowledgments and the
  catalog schema version reservation. Existing pairing records remain
  backward-compatible without a pairing document version bump.
- Coordinator reserved NVHTTP product-settings v17 for the combined parity
  batch. This slice does not modify product-settings or reserve a migration;
  its launch projection work does not conflict with v17.
- The host-configuration provider contract is read-only. No duplicate NVHTTP
  product settings or file-state persistence was added.
- `global:ds4_back_as_touchpad_click` was relayed with overlapping NVHTTP
  launch projection paths, but it is not in this lane's fixed ID assignment.
  Request an explicit ID/scope assignment before implementing that field.
- No additional work was completed for the remaining network IDs in this slice:
  address family, bind address, CSRF origins, external IP, FEC, LAN/WAN
  encryption, origin policy, ping-timeout parity, port/startup recovery,
  remote-monitor lifecycle, or UPnP.

## Next slice

Trace `global:address_family`, `global:bind_address`, and `global:port` from
NVHTTP product settings through listener creation and startup recovery. Make
only network-owned changes, record platform-specific listener behavior, and
keep all three IDs pending until coordinator integration tests qualify bind
and recovery behavior.


## Previous status: native worker model gate blocked (2026-09-24)

- CLI session: `01a0d516-8616-7270-a509-547be018f212`; lead model/effort: `gpt-6-luna/max`.
- Assignment branch: `config-parity/network`; original lane base: `30acd5de34ac5548ebf4d46086fc32ccad7b6913`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Current HEAD/base for the uncommitted candidate: `65b5a8cd23030427fe3b3c073239ef3fe99df833`. No candidate commit was created for this dirty worktree state.
- Requested two implementation workers as `gpt-6-luna/xhigh`. `encryption_runtime` reported GPT-6 with inherited/default medium effort; `network_listeners` reported GPT-6 and could not expose its exact effort. Neither worker inspected or edited files. Per the fixed assignment gate, source edits stopped after those reports.
- Worker support exists through the collaboration tool, but the tool did not surface the required worker model/effort. No model substitution was authorized.

The current uncommitted candidate touches these owned paths:

- `modules/nvhttp/src/launch.rs`
- `modules/nvhttp/src/launch/record.rs`
- `modules/nvhttp/src/launch/tests.rs`
- `modules/nvhttp/src/launch/workflow.rs`
- `modules/nvhttp/src/lib.rs`
- `modules/nvhttp/src/preparation.rs`
- `modules/nvhttp/src/product_settings.rs`
- `modules/nvhttp/src/wire.rs`
- `modules/nvhttp/src/frame_limiter.rs` (untracked)
- `modules/pairing/src/protocol.rs`
- `modules/rtsp/src/model.rs`
- `modules/stream-session/src/model.rs`
- `modules/stream-session/src/pipeline/tests/input.rs`
- `modules/stream-session/src/pipeline/workflow.rs`

The dirty worktree includes scoped stream-control support, raw global-preparation storage/launch export, frame-limiter persistence and partial launch projection, sparse client frame-limiter overrides, RTSP input override forwarding, and stream-session DS4 forwarding. These changes remain uncommitted and are not claimed as complete parity.

### Validation and platform evidence

- `git diff --check`: passed.
- `rustfmt --edition 2024 --check` over all changed Rust paths: failed with formatting diffs in NVHTTP, pairing, and stream-session files. No formatting writes were made after the worker-settings blocker.
- Rust tests, build, and Clippy: not run in this lightweight worktree.
- Platform qualification: none.
- Coordinator integration should run `cargo test -p amaterasu-nvhttp`, `cargo test -p amaterasu-pairing`, `cargo test -p amaterasu-rtsp`, `cargo test -p amaterasu-stream-session`, `cargo test -p amaterasu-media-transport`, `cargo test -p amaterasu-gamestream`, then Clippy for those packages in the reserved clean source checkout, followed by Windows runtime qualification.

### Remaining mismatches and next slice

- All 18 assigned network IDs remain `pending` in the coordinator-owned ledger.
- The single v17 product-settings migration currently serializes 29 values for legacy settings + raw preparation + frame-limiter fields. It does not yet include `lan_encryption_mode` and `wan_encryption_mode`; add them to that same v17 migration after the worker gate is cleared, then validate the resulting 31-value shape.
- Encryption runtime parity still needs actual-peer LAN/WAN classification, required media cipher enforcement for `never` and `mandatory`, and carrying negotiated audio/video encryption flags from RTSP ANNOUNCE into media startup.
- The latest frame-limiter contract requires a boolean `context.virtual_display`; the current NVHTTP projection has no authoritative source and leaves context incomplete. `client_display_mode_override` and `game_frame_generation_mode` also need their lifecycle owners to supply values; lane05 must enrich the launch snapshot with executable/session identity and application scope.
- The DS4 stream-session payload preserves optional false/true values, but retry identity still needs the exact `pipeline/record.rs` ownership/coordination response requested in outbox 0007.
- Listener address-family/bind/port behavior, CSRF/origin policy, external IP, FEC, ping timeout, UPnP, remote-monitor lifecycle, and remaining settings-editor/persistence parity have not been completed.
- At that checkpoint, the next slice was to resume after worker verification. The later isolated CLI worker verification and pairing dependency are recorded below; the 18 network IDs remain pending.
## Resolved dependency slice: pairing identity inline comments (2026-09-24)

- Request from Luna01; dependency scope is the legacy pkey and cert identity path parser. Stable IDs global:pkey and global:cert remain owned by Luna01.
- Only changed path: modules/pairing/src/migration.rs; tests are colocated in that file.
- Child codex exec session: 01a0d573-a41f-7110-9a42-08697b9415b9. Its persisted turn context and commit attribution helper confirmed model=gpt-6-luna, reasoning_effort=xhigh, and the isolated child checkout as its cwd.
- Isolated child worktree: D:/sources/worktrees/amaterasu-config-pairing-migration-child, branch codex/luna02-pairing-migration-child, base 65b5a8cd23030427fe3b3c073239ef3fe99df833.
- A metadata-only resume initially inherited the lane cwd and was stopped before edits. Implementation began only after resuming from the child cwd and confirming that cwd and gpt-6-luna/xhigh in the persisted turn context.
- Child source commit: bcea79ed6c71430b73807a1c253c66a06ff45395.
- Cherry-picked lane commit: a9c757026b9a5e72799d49aaa46a3fe9d582950b; current lane HEAD is this commit on config-parity/network.
- The parser now strips # comments outside double quotes and retains quoted path hashes. Existing relative-path, containment, regular-file, symlink, fallback, and ambiguity checks remain in place. Tests cover pkey/cert trailing comments, quoted hash filenames, commented traversal rejection, byte-for-byte source preservation, and exact imported PEM bytes.
- Child rustfmt --edition 2024, child git diff --check, staged diff check, lane rustfmt --edition 2024 --check modules/pairing/src/migration.rs, and lane git diff --check passed. Cargo tests/build/Clippy were not run; coordinator will validate packages in main. No platform qualification was run.
- The lane's unrelated uncommitted NVHTTP, RTSP, pairing protocol, stream-session, and handoff changes remain outside this commit. No files under those paths were changed by the child commit or cherry-pick.
- The existing db14b28e stream-control candidate remains unlanded. No Luna03 independent review was present in the checked lane inbox/outbox when this handoff was updated. Keep that candidate unlanded until its findings are resolved and coordinator integration tests run.
- Lane02's 18 assigned network IDs remain pending; this dependency slice does not close any of them.

### Next action

The child source and lane cherry-pick SHAs were sent to coordinator and Luna01 in outbox 0009. Continue waiting for Luna03's independent read-only review; do not land the stream-control candidate before review findings are addressed and integration tests complete.

## Global LAN/WAN encryption policy foundation (2026-09-24)

- Stable IDs: `global:lan_encryption_mode` and `global:wan_encryption_mode`, global scope. Both remain pending in the coordinator-owned coverage ledger.
- Lane code candidate: `1f1b50adf9aadaf5655149a78836dda2d7dd03f1`; isolated XHigh source commit: `3425a1db6d743ce2a3f897bda9047faa05097662`; child base: lane SHA `a9c757026b9a5e72799d49aaa46a3fe9d582950b`.
- Changed code paths: `crates/gamestream/src/lib.rs` and `crates/gamestream/src/network.rs`. The new helper normalizes IPv4-mapped IPv6, classifies local/LAN/WAN peers against the pinned network ranges, selects LAN or WAN mode, validates numeric modes 0..=2, exposes encryption support/requirement predicates, and supplies source defaults LAN=never and WAN=opportunistic. Tests cover classification boundaries, mapped IPv4 addresses, defaults, mode validation, and predicates.
- Behavioral source: pinned Vibeshine commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`, `src/network.cpp` and `src/config.cpp`. No source code was copied or translated line by line.
- Worker: codex exec thread `01a0d59a-2a3c-7631-a683-c5ae28338e98`; persisted startup and resumed turn contexts confirm model `gpt-6-luna`, effort `xhigh`, and cwd `D:\sources\worktrees\amaterasu-config-network-xhigh-encryption-classification-child`. Child branch `codex/luna02-encryption-classification-child` was clean at base and clean after its source commit. Configured Git identity was `Nonary <Chase_Payne@outlook.com>`.
- The lane cherry-picked the worker commit, then applied the repository's Rust 2024 import ordering to `network.rs` and amended the lane copy. Source SHA and lane SHA therefore differ only because of this formatting correction.
- Checks: child rustfmt and `git diff --check` passed; lane `rustfmt --edition 2024 --check crates/gamestream/src/lib.rs crates/gamestream/src/network.rs` passed after the formatting correction; lane `git diff --check` passed. Cargo tests, builds, and Clippy were not run in this lightweight worktree.
- Platform evidence: static IP classification only. Runtime behavior and Windows/Linux/macOS qualification were not run.
- Remaining work: persist both settings with defaults and validation; wire the selected policy into RTSP negotiation and stream-session media setup; reject unsupported video encryption under `never`/`mandatory` while preserving negotiated audio encryption; add the signed editor. No runtime consumer was changed by this slice.
- Version note: this update supersedes the earlier next-step note proposing to add these fields to v17. The coordinator's existing single NVHTTP v16-to-v17 reservation is for frame-limiter and preparation fields. Request an explicit new version allocation before changing product-settings migration; do not create another v17 migration.
- Coordinator routing relayed app/catalog/launch work to luna-05 and signed route/editor work to luna-06. Their acknowledgments and the catalog version reservation remain pending. The encryption editor request and NVHTTP version request are recorded in outbox 0010.
- The existing `db14b28e` stream-control candidate remains unlanded until Luna03's independent read-only review findings are resolved and coordinator integration tests pass.

### Next slice

Request a product-settings version decision and luna-06 acknowledgment for these two global fields. Once reserved, implement their durable NVHTTP persistence and validation, then connect the per-peer policy to RTSP negotiation and media startup without changing audio encryption behavior. Continue independent assigned network IDs while those dependencies are pending; keep every uncovered ID pending.

## Listener address-family default correction (2026-09-24)

- Stable IDs: `global:address_family` and `global:bind_address`; both remain `pending` in the coordinator-owned coverage ledger. This slice changes how explicit family policy supplies defaults when `bind_address` is omitted or blank. It preserves the legacy socket when both policy fields are omitted, and preserves explicit IP addresses.
- Isolated XHigh source commit: `8c46f1c495f055716b3166491a7a5032211cab3a`, based on lane SHA `de1c9355070125bcd2d0033db1b4f9a32a386567`. Reviewed lane commit: `d66b62c5500d54f5d31d03a3786f55fa5232cd6e`.
- Changed paths: `modules/nvhttp/src/settings.rs` and `modules/nvhttp/src/server.rs`. Explicit `address_family` now selects the requested family defaults for omitted, empty, or whitespace bind values, while retaining local-only versus remote-wildcard behavior. Legacy omitted policy fields continue using the legacy bind IP. Tests cover IPv4 and dual-stack resolution, empty values over legacy IPv6 binds, remote modes, and live IPv4 HTTP/HTTPS listeners.
- Implementation CLI thread: `01a0d5c3-dec1-7532-a4a9-3d463505519f`; persisted child turn contexts confirm `gpt-6-luna/xhigh` and the isolated implementation worktree. Source worker ran rustfmt and `git diff --check`; it ran no Cargo commands. Separate read-only reviewer CLI thread `01a0d5db-4dd1-7332-9d4f-8012b0b23b66` persisted `gpt-6-luna/xhigh` and returned no findings for the exact source commit.
- Lane checks: `rustfmt --edition 2024 --check modules/nvhttp/src/settings.rs modules/nvhttp/src/server.rs` and `git diff --check` passed. Cargo tests, builds, and Clippy were not run in this lightweight worktree. No platform runtime qualification was run.
- Coordinator validation should run `cargo test -p amaterasu-nvhttp` and `cargo clippy -p amaterasu-nvhttp --all-targets` in the reserved clean source checkout, then qualify IPv4/IPv6 listener creation on supported platforms. No product-settings migration or persistence change is included here.
- `global:port` remains outside this slice. The `db14b28e` scoped stream-control candidate remains unlanded pending Luna03's independent read-only review and coordinator integration tests. Existing dirty NVHTTP, pairing-protocol, RTSP, and stream-session changes were preserved.

### Next slice

Continue the independent NVHTTP and stream-session candidate work on its assigned paths. Keep any ID pending until persistence, authority, runtime behavior, retry scope, and platform evidence are reviewed.

## RTSP address-family default correction (2026-09-24)

- Stable IDs: `global:address_family` and `global:bind_address`; both remain pending in the coordinator-owned coverage ledger. This RTSP-side correction applies an explicit family when `bind_address` is absent, empty, or whitespace, and preserves legacy `bind` IP behavior when both policy fields are absent.
- Isolated XHigh source commit: `945d6c1e59813a8ec43ead0a3975ef7b7f0b4b2f`, based on lane HEAD `298ae080431e940c628a25743b49da4796d2c1a6`. Lane cherry-pick commit: `09a0be38417533092e7b332eeec271aa73cf77cd`.
- Worker CLI thread `01a0d5e9-8a2a-7092-9b06-eb18b83ff8f8`; persisted implementation turns confirm `gpt-6-luna/xhigh`, isolated cwd `C:\Users\Chase\.codex\worktrees\7bb7\amaterasu-config-network`, clean base `298ae080431e940c628a25743b49da4796d2c1a6`, and only `modules/rtsp/src/model.rs` changed. Commit body includes the required coordinator/planner attribution lines.
- The code uses the selected family's loopback or wildcard defaults for explicit policy and preserves explicit IP behavior. Colocated coverage exercises IPv4 and dual-stack, omitted/empty/whitespace bind addresses, both `allow_remote` states, and legacy IPv6 with no policy fields. The pre-existing dirty DS4 Open projection/tests in `modules/rtsp/src/model.rs` were stashed, restored unchanged, and remain uncommitted.
- `modules/rtsp/README.md` now documents the explicit-family default behavior. Lane `rustfmt --edition 2024 --check modules/rtsp/src/model.rs` and `git diff --check` passed. Cargo tests, builds, and Clippy were not run; platform listener qualification remains pending.
- Coordinator validation should run `cargo test -p amaterasu-rtsp` and `cargo clippy -p amaterasu-rtsp --all-targets` in the reserved clean source checkout, then qualify IPv4 and IPv6 listener behavior on supported platforms. Both IDs remain pending full NVHTTP/RTSP listener, persistence, management authority, and platform review.
- The `db14b28e` scoped stream-control candidate remains unlanded until Luna03's independent read-only review is returned, findings are resolved, and coordinator integration tests pass. `modules/pairing/src/migration.rs` was not changed.
## Effective max-bitrate precedence correction (2026-09-25)

- Stable ID: global:max_bitrate; remains pending full parity review. This
  correction removes the premature rejection based only on a global cap below
  100 kbps. The existing effective-cap validation still rejects a selected
  positive cap below the encoder minimum before RTSP opens.
- XHigh child commit: 3804271025196509b8631b63acc83e3603fb5046, based on lane
  bd5baf2db995dfb82acb62e0fb4a3992f31d5b0d. Reviewed lane candidate:
  be11030282ae9b067c3511819c65037719161b2a. Only
  modules/nvhttp/src/launch.rs and modules/nvhttp/src/launch/tests.rs are in
  the code commit. The child persisted CLI session
  01a0d5f6-7a9d-7342-be4a-b5ee6477c90d confirms gpt-6-luna/xhigh,
  isolated worktree, and Nonary identity.
- Focused cases now cover global 99 with client cap, application cap, explicit
  client no-cap, low effective client cap rejection before RTSP open, and global
  99 without a scoped override rejection. Lead review confirmed precedence is
  resolved before the effective-cap minimum check and before RTSP opens.
- Child rustfmt and git diff --check passed. Lane
  rustfmt --edition 2024 --check modules/nvhttp/src/launch.rs modules/nvhttp/src/launch/tests.rs
  and git diff --check passed. Cargo tests/build/Clippy were not run in the
  lightweight worktree. Coordinator validation should run
  cargo test -p amaterasu-nvhttp and
  cargo clippy -p amaterasu-nvhttp --all-targets -- -D warnings.
- Outbox 0018-bitrate-precedence-p2.json sends source and lane SHAs to the
  coordinator and Luna03 for independent re-review. Keep the scoped stream
  candidate db14b28e unlanded until Luna03's review findings are resolved and
  coordinator integration tests pass.
- The previously dirty launch projection/frame-limiter hunks were path-stashed
  during cherry-pick and restored without conflicts. All remaining dirty NVHTTP,
  pairing, RTSP, and stream-session edits remain uncommitted and preserved.

## Listener target apply and retry candidate (2026-09-25)

- Stable IDs: `global:address_family`, `global:bind_address`, and `global:port`; all remain pending full parity review. This candidate adds runtime application/retry logic for the complete persisted HTTP/HTTPS target. It wakes on any resolved-address change, preserves the active listener pair after a failed bind, and completes the apply only after RTSP confirms the same target.
- Isolated XHigh source commit: `88705593e615b14b8cf1f9d9b6cbf285d6e2a9dc`, based on lane SHA `ce99275ce422a60408aa1f6ef3af891592e5066a`. Reviewed lane commit: `620f93a23120f9ec57d968e8b347c792f5b34199`.
- Code commit path: `modules/nvhttp/src/lib.rs`. Worker CLI thread `01a0d605-fc13-70b0-80c9-923c6c67e5dd`; persisted startup and resumed turn contexts confirm `gpt-6-luna/xhigh`, isolated worktree, and clean base. The lead reviewed the source diff and cherry-picked the source commit without changing its code.
- Focused cases in the candidate cover IPv4 and dual-stack target derivation, invalid or occupied-bind failure retaining the listener pair, and retry success. Existing NVHTTP and RTSP endpoint owners perform the actual bind and target confirmation; the candidate does not qualify cross-platform behavior.
- Lane checks: `rustfmt --edition 2024 --check modules/nvhttp/src/lib.rs` and `git diff --check` passed. The entire remaining dirty Rust set also passes `rustfmt --edition 2024 --check`; its edits remain uncommitted. Cargo tests/build/Clippy were not run in this lightweight worktree. Coordinator validation should run `cargo test -p amaterasu-nvhttp` and `cargo clippy -p amaterasu-nvhttp --all-targets -- -D warnings`, then qualify IPv4/IPv6 listener rebind and occupied-bind recovery on supported platforms.
- Persistence, management edit authority/UI, and supported-platform behavior are still outstanding for these IDs. Do not mark them verified. Outbox `0021-nvhttp-port-rebind.json` sends the exact source and lane SHAs to the coordinator.
- The `db14b28e` scoped stream-control candidate remains unlanded pending Luna03's independent review, resolution of findings, and coordinator integration tests.

## Bitrate candidate integration dependency (2026-09-25)

- Coordinator inbox `coordinator-bitrate-candidate-rework.json` reports that main cherry-picked `be110302` without its parent feature support, the focused NVHTTP tests failed to compile because `peer_with_stream_overrides` and `Broker.application_max_bitrate` were absent, and that main reverted the attempt as `ca36e432`.
- The XHigh P2 source commit `3804271025196509b8631b63acc83e3603fb5046` is based on `bd5baf2db995dfb82acb62e0fb4a3992f31d5b0d`, which contains scoped client/application bitrate support and its peer/broker fixtures from `db14b28e5b0d1da50382f02b02052c4ebc67feb4`. Lane P2 commit `be11030282ae9b067c3511819c65037719161b2a` changes only `modules/nvhttp/src/launch.rs` and `modules/nvhttp/src/launch/tests.rs`.
- Do not apply `be110302` alone to main. The proposed integration order is `db14b28e` (the scoped-control dependency and fixtures), followed by `be110302` (the effective-cap validation correction). Validate the combined final tree after both commits; do not land the first commit while its reviewed P2 finding remains unresolved. The exact child/lane commits and required package checks are sent in outbox `0022-bitrate-dependency-sequence.json`.
- Luna03's immutable review of `db14b28e` found only the P2 bitrate issue. Its separate XHigh rereview of `be110302` found no behavioral findings. Coordinator compile/test validation of the ordered candidate is still required, so `global:max_bitrate` remains pending and the `db14b28e` candidate remains unlanded.

## RTSP ANNOUNCE encryption snapshot to PLAY (2026-09-25)

- Related stable IDs: `global:lan_encryption_mode` and `global:wan_encryption_mode`; both remain pending. This runtime handoff preserves the exact video/audio encryption booleans from an accepted ANNOUNCE through the later authenticated PLAY start. It does not select LAN/WAN policy, change media/audio policy, or add settings persistence.
- Lane base: `5e7f5a56813481fcf8ca2f5a9f4c781fb83db7d7`. The isolated XHigh RTSP source commit is `8bd5da26d79a7d5f9976752c00137ce486976f8a`, based on child HEAD `2d2ef8a44abfc4dd06ee89aaf64ba1ac992d8460`; reviewed lane cherry-pick: `c30b409253564602f07fa5f3a2b3b4857a96b2f6`. The lead-owned stream-session consumer commit is `8f45871b5365ad4a7548a92c24c75870567815e4`, based on lane commit `c30b409253564602f07fa5f3a2b3b4857a96b2f6`. Current code candidate: `8f45871b5365ad4a7548a92c24c75870567815e4`.
- Changed paths across the two code commits: `modules/rtsp/src/model.rs`, `modules/rtsp/src/controller/actions.rs`, `modules/rtsp/src/controller/record.rs`, `modules/rtsp/src/controller/network.rs`, `modules/rtsp/src/controller/tests.rs`, `modules/stream-session/src/model.rs`, `modules/stream-session/src/pipeline/actions.rs`, and `modules/stream-session/src/pipeline/tests/armed.rs`.
- The RTSP record retains flags only after the configured profile accepts ANNOUNCE; PLAY fails closed if that snapshot is absent and otherwise sends `configuration.encrypt_video` and `configuration.encrypt_audio`. Legacy `Open::arm` payloads remain unchanged. Strict stream-session `StartConfig` now accepts these fields optionally for older callers and rejects a provided value that differs from the immutable prepared encryption policy.
- Integration must apply the stream-session consumer commit `8f45871b5365ad4a7548a92c24c75870567815e4` before the RTSP producer commit `c30b409253564602f07fa5f3a2b3b4857a96b2f6`, then validate the combined tree. Both commits must be present before landing this feature.
- RTSP worker CLI thread `01a0d646-f94e-74a0-b638-d05a0b2d6d72` persisted `gpt-6-luna/xhigh`; its isolated worktree was clean at base and after commit. The lead's StartConfig contract change and test were completed in the lane's `gpt-6-luna/max` session `01a0d516-8616-7270-a509-547be018f212`. The worker changed only the five RTSP paths listed above.
- Focused tests cover accepted ANNOUNCE values flowing to the authenticated start payload, rejection clearing a stale snapshot and preventing that payload, backward-compatible legacy `Open::arm`, matching false/false StartConfig values, and mismatch rejection before arming. Tests were added but not run in this lightweight worktree. Rustfmt and `git diff --check` passed. Cargo tests/build/Clippy and supported-platform qualification were not run.
- Coordinator integration checks: `cargo test -p amaterasu-rtsp`, `cargo clippy -p amaterasu-rtsp --all-targets -- -D warnings`, `cargo test -p amaterasu-stream-session`, and `cargo clippy -p amaterasu-stream-session --all-targets -- -D warnings`. Runtime platform qualification remains outstanding.
- Remaining parity work for the two IDs includes persisted mode defaults/validation under the reserved NVHTTP v17 batch, selecting the LAN/WAN policy for each peer, enforcing policy support during negotiation while preserving current audio handling, and the signed management editor contract. These IDs are not verified by this slice. The existing `db14b28e` scoped stream-control candidate remains unlanded pending Luna03's independent review and coordinator integration tests. `modules/pairing/src/migration.rs` was not changed.

### Next slice

Continue the reserved NVHTTP v17 work using the coordinated field names and default contracts. Keep the stream-control dependency sequence and all remaining dirty frame-limiter, preparation, and DS4 work separated into independently reviewable commits.

## Pairing video batch-size semantic-validation correction (2026-09-25)

- Coordinator request `coordinator-luna02-pairing-override-test-fix-20260925` identified a failing pairing test that expected JSON deserialization to reject numeric `video_max_batch_size_kb=65`. This value deserializes into the field type; the configuration's semantic `validate()` rejects it.
- Isolated XHigh source commit: `2eb939dd4743bc6b185da1685ed0110db016251c`, based on lane HEAD `23355612bde78e19234d9ca6b1182e3f11508d7a`. Reviewed lane cherry-pick: `2d90b0319da825fcebf6718ca5b67b0e3b576294`. Only `modules/pairing/src/protocol.rs` changed. The test now parses `65` and asserts validation rejects it; wrong-type and signed bitrate-overflow deserialization checks remain.
- Worker CLI session `01a0d65d-d6a5-78a3-8c76-2a02f90181ee` persisted `gpt-6-luna/xhigh`, isolated worktree `D:/sources/worktrees/amaterasu-config-network-xhigh-pairing-override-test-child`, clean base `23355612bde78e19234d9ca6b1182e3f11508d7a`. Commit uses configured Nonary identity and required orchestration/planning footers.
- Worker checks `rustfmt --edition 2024 --check modules/pairing/src/protocol.rs` and `git diff --check` passed; the lane repeated both checks after cherry-pick and restoring the pre-existing frame-limiter override edits. Cargo tests/build/Clippy were not run in this lightweight worktree. `modules/pairing/src/migration.rs` was not changed.
- Coordinator validation: rerun `cargo test -p amaterasu-pairing` and `cargo clippy -p amaterasu-pairing --all-targets -- -D warnings` in the reserved clean source checkout, then continue the previously coordinated combined candidate checks as applicable. The test correction is ready for integration validation; no stable ID is marked verified here.
- The `db14b28e` scoped stream-control candidate remains unlanded pending the Luna03 read-only review resolution and coordinator integration validation. Existing dirty NVHTTP, pairing protocol frame-limiter, RTSP, and stream-session work remains preserved and uncommitted.
## LAN/WAN video-encryption ANNOUNCE policy helper (2026-09-25)

- Stable IDs `global:lan_encryption_mode` and `global:wan_encryption_mode` remain pending. This bounded GameStream slice validates the client's parsed ANNOUNCE video-encryption choice against a selected mode: never accepts only unencrypted, opportunistic accepts either choice, and mandatory accepts only encrypted video. It does not change audio negotiation or select the peer mode at runtime.
- Isolated XHigh source commit: `e9f367414f855172721bb566f359fabac74898fb`, based on `4416a5a645a4e6e3c2548103b14c82c575e18481`. Independent read-only XHigh review session `01a0d672-1bd6-7f21-95d2-3b755871eb0c` reported no findings. Reviewed lane cherry-pick: `cbcb00ffb7c703a09f04a01a34342bcb97aeaa84`. Only `crates/gamestream/src/network.rs` changed.
- Child and lane `rustfmt --edition 2024 --check crates/gamestream/src/network.rs` and `git diff --check` passed. Cargo tests/build/Clippy and platform runtime qualification were not run in this lightweight worktree.
- The coordinator's later decision `coordinator-v17-combined-batch-decision-01` reserves exactly one NVHTTP v16-to-v17 migration for frame-limiter globals and raw global preparation; `coordinator-nvhttp-v17-reservation-confirmation-20260924-01` also explicitly includes LAN/WAN encryption modes in that same migration. This supersedes the earlier v18 request in outbox `0010`; do not allocate or implement a separate v18 migration. The current dirty v17 settings candidate has not yet added encryption persistence.
- Remaining work: persist validated LAN/WAN modes with defaults 0/1 in the reserved v17 document; resolve LAN/WAN mode for the authenticated peer; apply this helper during RTSP ANNOUNCE matching; retain ANNOUNCE's selected video/audio booleans through PLAY and media startup; and coordinate the signed editor. IDs remain pending. The separate `db14b28e` scoped stream-control candidate remains unlanded until Luna03's independent review issue is resolved and coordinator integration checks pass. Existing dirty NVHTTP, pairing, RTSP, and stream-session changes remain preserved; `modules/pairing/src/migration.rs` remains untouched.

## Stream-session encryption-status test fixture correction (2026-09-25)

- Coordinator request `coordinator-stream-encryption-test-fixture-failure-20260925` reported that the test prepares video and audio encryption as false, so the media-open request carries `encryption: null`; the test broker indexed that null object and returned null status fields. The media status contract requires explicit booleans.
- Stable IDs related to this test: `global:lan_encryption_mode` and `global:wan_encryption_mode`; both remain pending. This slice changes only the test mock and does not change production behavior or weaken the immutable encryption mismatch guard.
- Isolated XHigh source commit: `b2f762e9bd985c529a7e2ccfc45ca4c9ccab029d`, based on lane SHA `397a068be58c015fd1da2dfc6f7194ed58ff73f7`. Reviewed lane cherry-pick: `0b2993014c3ccc3fad121c23b71cfd3c5952792f`.
- Only changed path: `modules/stream-session/src/pipeline/tests/harness.rs`. The mock now converts each optional encryption status to a boolean and defaults absent/null values to false. The existing matching/mismatch test in `pipeline/tests/armed.rs` is unchanged.
- Worker CLI session `01a0d67d-9a7a-7f72-9b1c-1cf0701b81f0` persisted `gpt-6-luna/xhigh`; process cwd and Git root were verified as the isolated worktree `D:/sources/worktrees/amaterasu-stream-session-fixture-xhigh-verified`, branch `codex/luna02-stream-session-fixture-xhigh-verified`, clean at base and after commit. Configured Nonary identity and required orchestration/planning commit footers were verified.
- Worker and lane `rustfmt --edition 2024 --check modules/stream-session/src/pipeline/tests/harness.rs` and `git diff --check` passed. Cargo tests/build/Clippy were not run in this lightweight worktree. Coordinator validation requested: `cargo test -p amaterasu-stream-session`, `cargo test -p amaterasu-rtsp`, and `cargo clippy -p amaterasu-stream-session -p amaterasu-rtsp --all-targets -- -D warnings` in the reserved clean source checkout. No platform runtime qualification was run.
- This exact fixture correction is ready for coordinator cherry-pick and integration validation. Keep the encryption IDs pending. The `db14b28e` scoped stream-control candidate remains unlanded pending Luna03's independent review resolution and coordinator integration tests. Existing dirty NVHTTP, pairing-protocol, RTSP, and stream-session work is preserved; `modules/pairing/src/migration.rs` remains untouched.

### Next slice

Continue an independent network-owned RTSP/media runtime slice while the coordinator validates this fixture and the NVHTTP v17 dependencies remain coordinated.

## NVHTTP v17 combined persistence candidate (2026-09-24)

- Lane code candidate: `9341e6e19452ee3d84a2ad564865ad4fcda243a1`, based on `0bdf1d91ae0e65cb503803b824c149001223e6dc`; source baseline remains `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`. The candidate is committed on `config-parity/network`.
- This implements the coordinator's single additive NVHTTP v16-to-v17 migration for the combined batch. It preserves v16 values and structured `global_preparation`, while storing the distinct `legacy_global_preparation`, global frame-limiter values, and `lan_encryption_mode` / `wan_encryption_mode` in the v17 document. The resulting document has 31 values.
- Encryption mode values are integers 0 through 2. Defaults are LAN `0` (never) and WAN `1` (opportunistic). In `settings_update`, omitted mode fields preserve their stored values; explicit `null` resets the field to its source default; invalid integers and non-integer JSON values fail before persistence. Successful readback from `settings_get` and `settings_update` has flat `lan_encryption_mode`, `wan_encryption_mode`, and `revision` fields.
- Code paths in this candidate: `modules/nvhttp/src/frame_limiter.rs`, `modules/nvhttp/src/lib.rs`, `modules/nvhttp/src/preparation.rs`, and `modules/nvhttp/src/product_settings.rs`. The committed `lib.rs` hunk adds v17 settings update/readback handling; the current unstaged `lib.rs` hunk remains the separate launch projection and is preserved outside this commit.
- Focused coverage was added for mode omission/null/value decoding, reset/preserve/range validation, v17 migration and restart roundtrip, and flat readback. `rustfmt --edition 2024 --check` passed on the four committed Rust files; `git diff --check` passed on the lane working diff. Cargo tests, builds, and Clippy were not run in this lightweight worktree. Coordinator validation should run `cargo test -p amaterasu-nvhttp` and `cargo clippy -p amaterasu-nvhttp --all-targets -- -D warnings` in the reserved clean source checkout. No platform runtime qualification was run.
- An independent read-only reviewer session `01a0d6ab-96ce-7f41-93ef-b3ef61ac997d` persisted `gpt-6-luna/xhigh` and found no findings in the encryption edit flow. The lead implementation session is `01a0d516-8616-7270-a509-547be018f212` (`gpt-6-luna/max`).
- The coordinator confirmed that L06 owns the signed settings route/editor and requested the exact persisted response shape. Outbox `0029-v17-encryption-settings-contract.json` sends the field names, defaults, validation, reset semantics, candidate SHA, and coordinator gates to the coordinator and L06; no L06 route or UI path is changed here. L03 and L05 remain the owners of their frame-limiter runtime and preparation behavior.
- Remaining encryption gaps: launch projection of the selected modes, actual peer-based policy selection, RTSP ANNOUNCE enforcement, and coordinator-approved media preparation ordering. Keep the immutable stream-session encryption mismatch guard. RTSP currently prepares media before ANNOUNCE, so do not accept a different encryption choice after preparation until the cross-lane contract settles that ordering. The signed editor and Windows/runtime qualification are also pending; both encryption IDs remain pending.
- The pre-existing unstaged NVHTTP, pairing protocol, RTSP model, and stream-session work remains untouched by this code commit. `modules/pairing/src/migration.rs` remains untouched. Keep `db14b28e` unlanded until Luna03's independent review findings are resolved and coordinator integration tests pass.

### Next slice

Continue only with an independent, version-approved network slice or an already-reserved launch projection. Do not expand v17, modify the encryption mismatch guard, write L06 editor paths, or land `db14b28e` without the required dependency and integration gates.

## Coordinator main validation: scoped stream controls (2026-09-25)

- Luna03's independent read-only review found one P2 in `db14b28e`: the global
  bitrate minimum was checked before client/application precedence. The focused
  correction is source commit `be11030282ae9b067c3511819c65037719161b2a`; the
  required four precedence cases were added there. The pairing semantic-test
  correction is `2d90b0319da825fcebf6718ca5b67b0e3b576294`.
- The coordinator landed the ordered source commits
  `db14b28e5b0d1da50382f02b02052c4ebc67feb4`,
  `be11030282ae9b067c3511819c65037719161b2a`, and
  `2d90b0319da825fcebf6718ca5b67b0e3b576294` on main as
  `592c479f279f967304826f7513d995143c0cf11d`,
  `ce27aa5550a63e12d366d6af6be2f12abf62d9a4`, and
  `ca207e527ff39de638bd3f8a387ddcbc2cd278f3`, respectively. At validation
  tip `ca207e527ff39de638bd3f8a387ddcbc2cd278f3`, format passed, NVHTTP passed
  163/163, pairing passed 37/37, RTSP passed 67/67, and strict Clippy passed
  for all three packages. This resolves the review/integration hold for the
  main landing. The later main tip is `0330b471965fabf026cd86173d1a3cc0480f186e`.
- These are coordinator source-checkout results; no Cargo tests, builds, or
  Clippy were run in this lightweight lane. The full configuration parity for
  `global:max_bitrate` and related stream-control IDs remains pending editor,
  persistence, and runtime qualification. The coordinator-owned coverage ledger
  remains unchanged.
- At lane head `7d3aadf87339c265460d3d4e5cc2701a2fdb3cb0`, the pre-existing
  uncommitted NVHTTP, pairing protocol, RTSP model, and stream-session launch
  projection paths remain preserved. `modules/pairing/src/migration.rs` remains
  untouched. The independent UPnP worker is isolated to `modules/upnp/**`.

### Next slice

Finish the isolated UPnP lifecycle candidate and request L01 workspace/version
coordination. Keep the AV1/audio projection work pending until its exact path
reservation and the existing dirty launch changes can be integrated safely.

## Coordinator v17 NVHTTP gate recheck (2026-09-25)

- The stream-control gate record above is for main validation tip
  `ca207e527ff39de638bd3f8a387ddcbc2cd278f3`. That commit predates the later
  v17 persistence commit `7e16e7fe5aae39cb528f5fdb2732ddaae3b038c7`; its
  163/163 NVHTTP test result does not validate the v17 product-settings code.
- At current main tip `095a15e2`, the coordinator reran
  `cargo test -p amaterasu-nvhttp`. Compilation still fails in
  `modules/nvhttp/src/product_settings.rs:1794` because the flat settings JSON
  exceeds the `serde_json::json!` macro recursion limit. No current NVHTTP test
  or Clippy pass is claimed for the v17 candidate.
- The v17 gate remains blocked pending a focused, committed correction in the
  owning NVHTTP path and coordinator reruns of formatting, package tests, and
  strict Clippy. Preserve the existing flat readback fields, revision behavior,
  defaults, and omitted-versus-null update semantics during that correction.

## UPnP cleanup retry tests (2026-09-25)

- Worker source commit: `f41ed04198a75993f0e915fec08c146f8a6985e7`, based on lane handoff `5997e5b1e5a722a93dbd7bbda2e6f11dd4230590`. Worker session `01a0d6f9-8841-7c31-ae58-1ee867472a3d` was verified as `gpt-6-luna/xhigh`; exact path: `modules/upnp/src/tests.rs`. Reviewed lane commit: `5649771939532989ad6a28b9839fd0e17f1341b0`.
- Two deterministic tests cover stale-mapping cleanup failure followed by successful reconcile retry, and shutdown failure followed by successful cleanup retry. They assert failed mappings remain tracked as owned and desired mappings remain active. The fake gateway gained only a removal-failure setter; the UPnP crate has nine deterministic fake-gateway tests total.
- The lightweight worker ran edition-2024 rustfmt and `git diff --check`; Cargo tests/build/Clippy and router/platform qualification were not run there. Its handoff was written before root registration and coordinator package validation.

## Coordinator validation update: UPnP workspace and package gates

- L02 package commit `16ce924f22f3a942e4cd0a7ec21d4d961cae745c` landed as `fcf72de1`; cleanup retry tests `5649771939532989ad6a28b9839fd0e17f1341b0` landed as `ea1729a5`. L01 registration candidate `7472c8e67fa30ab5056bcf238887aa7f52b7438d` landed as `b8703e0b` with only root workspace membership and lockfile resolution; `igd-next` is pinned at 0.17.1 with default features only.
- Main gates passed: `cargo fmt --check -p amaterasu-upnp`, `cargo test -p amaterasu-upnp` (9/9), and `cargo clippy -p amaterasu-upnp --all-targets -- -D warnings`.
- `global:upnp` remains pending product-settings migration, management authority/editor integration, listener lifecycle wiring, lease refresh, router testing, and platform qualification.

## UPnP caller-driven lease refresh and IGDv2 countdown fix (2026-09-25)

- Isolated XHigh worker final source commit: `9d2ff0bbe1652198cf83cd23aeac41aa0b684a57`, based on lane `372b1675ced025863d1681ceaf36c72d8dc6705c`; it supersedes the worker's initial `5028f346` commit, which was not integrated. Worker session `01a0d702-cec9-7411-abfe-68a32d2920d3` persisted `gpt-6-luna/xhigh`, verified from the session JSONL before edits. Reviewed lane commit: `755a51352dc9211426c4215c7a85ef56e6f87de9`.
- Exact paths: `modules/upnp/src/lib.rs`, `modules/upnp/src/igd.rs`, `modules/upnp/src/tests.rs`, and `modules/upnp/README.md`. `MappingManager::refresh_owned` refreshes currently owned entries; missing/changed entries are relinquished and refresh errors retain ownership for retry. The adapter checks stable attributes before refreshing the recorded tuple; the IGD check-then-act race remains documented. The caller should use the exposed 30-minute recommended interval; this crate has no timer/runtime dependency.
- IGDv2 returns remaining lease seconds, which count down. Stable identity comparison ignores only that dynamic field and still compares ports, protocol, target address/port, remote host, enabled flag, and owner description. The regression covers the countdown across reconcile, refresh, and shutdown. A zero lease request is IGDv2's maximum 604,800 seconds (one week), so refresh must occur before expiry. The package now has 13 deterministic fake-gateway tests.
- The lightweight worker passed Rust 2024 rustfmt, `git diff --check`, and committed-diff checks; it did not run Cargo tests/build/Clippy, router behavior, or platform qualification.

## Coordinator validation update: UPnP lease refresh

- Source commit `755a51352dc9211426c4215c7a85ef56e6f87de9` landed on main as `ad0f72bb`. Main formatting passed, package tests passed 13/13, and strict Clippy passed.
- `global:upnp` remains pending listener refresh scheduling, product-settings migration, management authority/editor integration, router testing, and platform qualification. The package exposes refresh but does not add a timer.

## Coordinator validation update: v17 NVHTTP and continuous audio (2026-09-25)

- The reviewed continuous-audio sender commit `ff136dd8667b23af5f8f81e46febcaa1f7eb3e80` landed directly on main as `4e8f2df24f31824607576029282032b35c2a0a64`. L04's independent read-only review found no issues against the receiver contract. The change captures the global `stream_audio` value at admission, applies client/application/global precedence, forwards `continuous_audio` only when audio is effectively enabled, and carries the prepared 5 or 10 ms packet duration through RTSP and stream-session.
- The initial main package build exposed a crate macro recursion limit and stale v16 test fixtures. Coordinator commit `96312d7` raises the limit and aligns fixture map keys, numeric typing, and update confirmations with settings v17. Formatting passed and `cargo test -p amaterasu-nvhttp -p amaterasu-rtsp -p amaterasu-stream-session` passed.
- Strict Clippy passed for RTSP and stream-session. NVHTTP strict Clippy still reports four unused settings accessors that L02's pending consumers should use, the large `Purpose::Update` variant, boolean assertion style in `frame_limiter.rs`, and four cloned-reference slice lints in product-settings tests. These findings were sent to L02; no lint suppression was added.
- The sender depends on the existing v17 `stream_audio` value and does not itself complete `global:stream_audio` parity. Management/editor projection and override qualification, Windows audio behavior, and confirmation of remaining persistence requirements are still pending. No stable ID is closed by this validation update.

## Coordinator validation update: external IP transaction readback (2026-09-25)

- L02 worker source `753a167a378d0253f0062d406d82cc3030d8d0ae` and lane code `4534980127e2baf41a8e2a2325ce91aacd747593` landed directly on main as `1f56333eeb27b6c0a486ba143422150da08dd51a`. The test drives the revisioned settings update, exact whitespace-padded `external_ip` value, v17 confirmation/readback, and a fresh `ProductSettings` reload.
- `cargo fmt --check -p amaterasu-nvhttp` and `git diff --check` passed; `cargo test -p amaterasu-nvhttp` passed 181/181.
- Strict Clippy remains red on four unused accessors awaiting their consumers, the large `Purpose::Update` variant, four boolean comparison assertions in `frame_limiter.rs`, and four cloned-reference slice assertions in product-settings tests. Exact findings were sent to L02 for a worker-owned correction; no suppressions were added.
- `global:external_ip` remains pending effective address detection and advertisement, signed route/editor coverage, and platform/runtime qualification. This test closes no stable ID.
