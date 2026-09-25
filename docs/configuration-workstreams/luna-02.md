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
