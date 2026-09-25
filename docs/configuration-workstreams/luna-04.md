# Luna-04: audio and input configuration parity

## Current bounded slice: Console-role virtual-sink fallback

- Path reservation: inbox `031-virtual-sink-console-fallback-path-reservation.json`; request ID `luna04-virtual-sink-console-fallback-reservation-031`. Lane base: `b46286ad3f4217ddb8f12cda2ed4fd7765fccfa2`.
- Stable ID in scope: `global:virtual_sink`; it remains pending. Exact code paths: `modules/audio/src/virtual_sink.rs` and `modules/audio/src/platform/windows.rs`. These do not overlap the held continuous-capture candidate paths listed below.
- Worker thread `01a0d6be-e163-73a2-bd16-2e8d5257aada` was verified from `state_5.sqlite` as `gpt-6-luna`/`xhigh` before edit authorization. Isolated worktree: `D:/sources/worktrees/amaterasu-audio-virtual-sink-console-fallback-xhigh`, branch `codex/luna04-audio-virtual-sink-console-fallback-xhigh`, clean at the exact lane base. Worker source commit: `efb4dce47b8c2184a5cf79ac9556e1ae10643d76`; reviewed lane code commit: `928c3d963727c6ad8100ee9a920ea00aae6a589d`.
- For host audio with an empty/default regular selector, automatic Steam virtual-sink fallback now checks whether the Windows Console render endpoint is active. Capture-only retains its prior Multimedia-based availability check. Host-audio-disabled behavior, explicit ordinary selectors, and explicit virtual selection including `Some("")` remain unchanged. No schema, persistence, or management path changed.
- Focused pure regressions cover Console/Multimedia availability disagreement in both directions, host-audio-disabled and explicit ordinary selection, capture-only behavior, and the empty explicit virtual selector. Worker and lane `rustfmt --edition 2024` checks and `git diff --check` passed. Cargo tests/build/Clippy were not run in this lightweight lane; authored tests remain unexecuted.
- Lane code candidate `928c3d963727c6ad8100ee9a920ea00aae6a589d` landed as `f4ed86b8`; the handoff landed as `00c1f4b5`, preserving main's cumulative history and labeling continuous framing as held.
- Main validation passed `cargo fmt --check -p amaterasu-audio`, `cargo test -p amaterasu-audio` (54 passed, 0 failed, 1 controlled-hardware test ignored), and strict Clippy.
- Windows COM role lookup and real endpoint-switching/hardware behavior were not qualified. Durable settings/default migration, scoped client/application resolution, management authority/UI, and full end-to-end audio evidence remain open. No stable ID is closed; all 23 assigned IDs remain pending.

## Held lane candidate: opt-in continuous capture framing (not yet picked by the coordinator)

- The preceding two-slice handoff landed on main as `06e44f2266d83a48249b266b8d8a5c8e8434cdc0`; the input and audio source commits and their passing package gates are recorded in the coordinator validation section below.
- Lane base: `b54e7c90da91c7c1ae66b063ff3acef8d1243d6d`. Reviewed worker commits `845b7bb005897022ee04c21bb31cce0b761793a1` and `4c575936a1c88bcd12a5dd80dbf35a66017c5b5c` were cherry-picked as lane commits `80d3548355dde55978799eb6429f8792d3ec6879` and `0c0942782eff83553695144b47bb0fa89ea4cff6`, respectively. The latter remains an unpicked lane candidate; do not edit its paths until the coordinator confirms its direct-main pick.
- The implementation thread `01a0d679-905a-7a12-b258-a81f05f63e44` was verified from `state_5.sqlite` as `gpt-6-luna` at `xhigh`, running in isolated worktree `D:/sources/worktrees/amaterasu-audio-input-next-slice-xhigh`, branch `codex/luna04-audio-input-next-slice-xhigh`, based on the exact committed lane base. The worker committed its changes; lead review confirmed both commits, required plan trailers, and audio-only path scope before cherry-picking.
- Changed paths: `modules/audio/README.md`, `modules/audio/src/controller.rs`, `modules/audio/src/controller/tests.rs`, and `modules/audio/src/model.rs`. `worker.rs` and `packet.rs` are unchanged because the framing is pull-driven at the controller read boundary and introduces no new worker wait or packet queue lifecycle.
- The `amaterasu.audio/1` Open request now accepts optional `continuous_audio: { packet_ms }`; absent or null retains the previous idle `MEDIA_PENDING` behavior. The supported 5 ms and 10 ms frame sizes use integer `sample_rate * packet_ms / 1000` frame sizing with fractional frames truncated. Continuous reads combine short captures, split oversized captures, and zero-pad partial data after the frame deadline. Wholly synthetic frames carry `AUDIO_SILENT`; idle synthetic timestamps are marked estimated. Sequence/timestamp continuity and capture discontinuity flags are preserved. The continuous policy participates in retry identity, and closing the stream drops its packetizer.
- Regression tests were added for missing/null legacy behavior, idle silent frames, partial padding, oversized packet splitting, packet-clock flags, fractional frame sizing, retry identity, and stream close. These tests were not run in the lightweight lane. `rustfmt` on the changed Rust files and `git diff --check` passed; Cargo tests/build/Clippy were not run.
- This adds an audio-provider capability only. The launch-to-audio-open consumer contract remains pending L02 acknowledgment: the proposal in outbox `029-continuous-audio-contract-proposal.json` asks L02 to snapshot Moonlight `continuousAudio` as a default-false session flag, keep it false when `stream_audio` is disabled, and pass the negotiated audio packet duration to Open. No matching acknowledgment is present in the inspected L04 inbox. Do not edit L02 consumer paths until coordinated.
- `global:stream_audio` is adjacent and unchanged; `continuousAudio` is a per-session launch flag, not a new persisted stable ID. No stable ID is closed. All 23 assigned IDs remain pending full persistence/default migration, scoped client/application resolution, management authority, end-to-end launch integration, and platform evidence. No controlled Windows audio hardware qualification was run.

## Coordinator validation: Windows gamepad lint and audio route retry landed

- The input helper candidate `bca6a3b203ea0b87d2fcfe1d4942b7163a179929` landed directly on main as `2f1690f396fa80b1a6259f1f141f324f311ad7fd`. Coordinator gates passed: `cargo fmt --check -p amaterasu-input`, `cargo test -p amaterasu-input` (113/113), and strict Clippy.
- The Windows route restoration retry candidate `6d5dec5bcd17eeb078be0ff0538af83c649b82e6` landed directly on main as `331f5968b5abe6ab3cabf4172336e80d2b0d26bc`. Coordinator gates passed: `cargo fmt --check -p amaterasu-audio`, `cargo test -p amaterasu-audio` (53/53; one controlled-audio hardware test ignored), and strict Clippy.
- Windows COM behavior and controlled endpoint route restoration remain unqualified. Durable audio settings/default migration, scoped client/application resolution, management authority/UI, and end-to-end stream evidence also remain open. Keep all assigned audio IDs pending.
- This status update does not alter either landed code slice. The lane head before this update was `62867867530328dd306ae8eeeca6197533d9fa20` on `config-parity/audio-input`; the coordinator owns direct-main cherry-picks.

## Coordinator validation: capture-only Console-default pinning landed

- Lane candidate `d4125c364c5fa5c1ee98e1a4eb3bdc51c40dacbb` landed directly on main as `9625a2f6f3ed4ffca27abcdeb573769e02303164`. Coordinator acknowledgement `coordinator-capture-only-contract-ack-20260925` identifies main tip `ca36e432c64ff1956a0b917d79dd19a54e129688` and accepts the policy.
- Accepted contract: when `capture_sink_without_routing` is true and the regular endpoint selector is empty/default, resolve and pin the Console endpoint once; retain it across later default changes and device-loss retries. With the flag false, an empty/default selector follows Console changes. Explicit endpoint IDs and explicit/automatic virtual endpoint behavior are unchanged. No schema or migration change is needed for this slice.
- Coordinator validation on the landed candidate passed `cargo fmt --check -p amaterasu-audio`, `cargo test -p amaterasu-audio` (51 passed, 0 failed, 1 hardware test ignored), and strict Clippy. No Windows endpoint-switching hardware qualification was run.
- The candidate changes only the audio platform policy and README paths recorded in the bounded-slice section below. The next lane handoff update is based on lane HEAD `a9c9e43d56731a8d4a24b980a950a5ed51ba2f5b`; this documentation-only status commit does not alter the landed code.
- Persistence/default migration, scoped client/application resolution, management authority/UI, end-to-end stream integration, and Windows endpoint-switching qualification remain incomplete. Keep `global:audio_sink_capture_only`, `global:audio_sink`, and `global:virtual_sink` pending until integrated evidence supports closure.

## Previous bounded slice: capture-only Console-default pinning

- Lane code commit: `d4125c364c5fa5c1ee98e1a4eb3bdc51c40dacbb`; isolated worker source commit: `795c6e728f3da04f34e044339e714cf6aab43d57`; worker/lane base: `5db314c2cf3e655305ed5b51f0f55b9f430f273b`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable IDs in scope: `global:audio_sink_capture_only` and `global:audio_sink`. Both remain pending.
- Worker session/thread `01a0d615-e356-7ca2-9b30-98cebb377861`, verified `gpt-6-luna`/`xhigh` from the live thread row. The isolated worktree was `D:/sources/worktrees/amaterasu-capture-only-default-pin-xhigh`, branch `codex/luna04-capture-only-default-pin-xhigh`, clean at the required base before edits.
- Changed paths: `modules/audio/src/platform/mod.rs`, `modules/audio/src/platform/windows.rs`, `modules/audio/README.md`. Lead review confirmed only these paths changed and the worker commit has the required plan trailers.
- Capture-only opens using an empty/default regular selector now resolve the current Console render endpoint once at open, then disable Console-default following and reopen that same endpoint after device loss. Normal empty/default opens still follow Console changes. Explicit endpoint IDs and explicit/automatic virtual-sink selection remain unchanged. No API fields, launch payloads, persistence/default versions, or foreign-owned paths changed.
- Added a focused platform-policy test separating initial Console selection from follow behavior. README selection and recovery descriptions now state that capture-only default opens remain pinned.
- Worker and lane `rustfmt --edition 2024 --check` and `git diff --check` passed. Cargo tests/build/Clippy were not run in the lightweight worktrees; no Windows endpoint-switching hardware qualification was run.
- The behavior is supported by pinned Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`: its capture-only path selects `set_capture_sink`, opens by the resolved device ID, and disables default-change following. Coordination notice is in outbox `022-capture-only-default-pin-implementation-notice.json`; coordinator/L02 acknowledgement is requested before direct-to-main landing.
- Persistence/default migration, scoped resolution, management authority, end-to-end stream integration, and platform qualification remain incomplete. Keep all audio IDs pending.

## Previous coordinator validation: active endpoint status contract

- Current main: `a582eee8e05b8a7fa3fc235b95fc1728f88d756e`. Lane branch: `config-parity/audio-input`, current lane base before this handoff update: `184840a84d69fc4d7582d212b2b1fe18bfbef59e`.
- Direct-main code landings: lane `f753d95f002476f2275a972944827725b2c0a4be` landed as main `5479dda5fe2f357ebd9ae0dce8aeb893487d4b1a`; lane correction `088818e502af98622f7793cbc8bf35df73f1aef4` landed as main `44c8ceb7f074163abfae98323905ccca665a6b08`.
- Direct-main handoff landings: lane `77076c16bf582d65bff4130e312102c05b2c6d50` landed as main `270847baf7f735f46b7bbc83467f3d54efa2dbb6`; lane `184840a84d69fc4d7582d212b2b1fe18bfbef59e` landed as main `a582eee8e05b8a7fa3fc235b95fc1728f88d756e`.
- Approved status contract: status reports the currently active capture endpoint; open and lookup keep the immutable initial endpoint for replay and shared-lease identity. The inspected stream-session consumer uses open and does not call audio status.
- Final coordinator validation on main: `cargo fmt --check -p amaterasu-audio` passed; `cargo test -p amaterasu-audio --target x86_64-pc-windows-msvc` passed (50 passed, 0 failed, 1 hardware test ignored); strict Windows-target Clippy passed (`cargo clippy -p amaterasu-audio --target x86_64-pc-windows-msvc --all-targets -- -D warnings`).
- Windows capture-endpoint switching/hardware qualification was not performed. All assigned audio IDs remain pending full persistence/default migration, scoped settings, management authority, end-to-end stream integration, and platform evidence.

## Previous bounded slice: active endpoint test correction

- Lane code correction: `088818e502af98622f7793cbc8bf35df73f1aef4`; isolated worker commit: `f7ba78911b348a2698c9625b1b89c8a509df106a`; worker/lane base: `77076c16bf582d65bff4130e312102c05b2c6d50`.
- Main candidate under correction: `5479dda5fe2f357ebd9ae0dce8aeb893487d4b1a`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`; branch: `config-parity/audio-input`.
- Stable ID: `global:audio_sink`, still pending. The test-only correction does not close any stable ID.
- Worker session `01a0d516-8713-7b83-87f1-d35c0828ae10`, thread `01a0d605-4aee-7e83-b3f2-a2e0bf957e06`, verified `gpt-6-luna`/`xhigh` from the live thread row. Isolated worktree `D:/sources/worktrees/amaterasu-active-endpoint-test-correction-xhigh`, branch `codex/luna04-active-endpoint-test-correction-xhigh`, clean at the required base before edits.
- Only `modules/audio/src/worker.rs` changed, and only test assertions: pinned `device_loss_retries_open_and_preserves_packet_clock` expects `render.fixed`; `console_default_change_reopens_new_endpoint_without_breaking_packet_clock` expects `render.console-new`. Production behavior and both assertions are preserved. Lead review confirmed the exact path and required plan trailers before cherry-pick.
- Worker and lane `rustfmt --edition 2024 --check modules/audio/src/worker.rs` and `git diff --check` passed. No Cargo tests/build/Clippy ran in the lightweight lane.
- Initial coordinator validation of main `5479dda5` found the misplaced test assertion described above. The test correction landed directly on main as `44c8ceb7f074163abfae98323905ccca665a6b08`; final formatting, Windows-target test, and strict Clippy results are recorded in the current coordinator-validation section.
- The coordinator approved the outbox 017 status contract: status reports the currently active capture endpoint, while open and lookup retain the immutable initial endpoint for replay and lease identity. The inspected stream-session consumer uses open and does not call audio status; no management status consumer was found.
- Persistence/default migration, scoped settings, management authority, complete stream evidence, and platform qualification remain incomplete. Keep all audio IDs pending.

## Previous bounded slice: active endpoint in audio status

- Lane code commit: `f753d95f002476f2275a972944827725b2c0a4be`; isolated worker source commit: `7041b6545df4c405ef4709375f8528a2b1690e6b`; worker and lane base: `b8f3a1596e215dbd4ae54cb00381a619072033ba`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable ID in scope: `global:audio_sink`. It remains pending. `global:audio_sink_capture_only` and `global:virtual_sink` were not changed and remain pending.
- Verified worker session `01a0d516-8713-7b83-87f1-d35c0828ae10`, thread `01a0d5f5-a807-75c1-bb2a-c49f4ad12bd8`, model/effort `gpt-6-luna`/`xhigh`. The child used isolated worktree `D:/sources/worktrees/amaterasu-audio-active-endpoint-status-xhigh`, branch `codex/luna04-audio-active-endpoint-status-xhigh`, and confirmed its clean base immediately before edits.
- Changed source paths: `modules/audio/src/model.rs`, `modules/audio/src/worker.rs`, `modules/audio/src/controller.rs`, `modules/audio/src/controller/tests.rs`, and `modules/audio/README.md`. Lead review confirmed the child commit's exact path ownership and required plan trailers before cherry-pick.
- Internal worker health now tracks the active capture endpoint in a serde-skipped field, sets it from the initial capture, and updates it only after a Console-default-follow reopen passes endpoint/format acceptance. Status uses that value through its existing `endpoint` field, falling back to the opening snapshot before worker initialization. Open and lookup continue to return the immutable initial endpoint and original `requested_endpoint`; the response shape is unchanged.
- Regression coverage updates the followed-default recovery worker test to assert the reopened endpoint and adds controller coverage showing status moves to the new endpoint while repeated open and lookup retain the initial endpoint. Explicit endpoint IDs stay pinned; explicit/automatic virtual sink and capture-only routing policy code is unchanged.
- Worker and lane checks: `cargo fmt --all -- --check` and `git diff --check` passed. No Cargo tests, build, or Clippy ran in the lightweight lane. Windows endpoint switching/hardware qualification was not performed.
- Cross-lane status-semantics request was recorded in outbox `017-audio-active-endpoint-status-contract.json`. The coordinator has since approved the distinction between active status and immutable open/lookup identity and confirmed the inspected stream-session path uses open, not audio status.
- Remaining parity at that point included persistence/default migration, scoped client/application resolution, management edit authority, full stream integration and Windows hardware qualification. The capture-only default-pinning slice is now recorded above; inspect the remaining virtual-sink gaps next. Keep all audio IDs pending.

## Current bounded slice: open_selected Clippy correction

- Lane code commit: `7651a3c049a45936c1dae5ccb48ad7e817f33f29` (`refactor(audio): group selected capture options`); isolated worker source commit: `3904a8a70160cc4ab333e778fcb82a5440335d86`; base: `d71ba1db9d89574abcd9b396befe965e874e0ca8`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable IDs and runtime behavior are unchanged by this lint correction. The Console-default-follow slice remains pending broader parity review.
- Verified worker process session `01a0d516-8713-7b83-87f1-d35c0828ae10`, thread `01a0d5e8-d504-7f12-9f92-9592dc8565b5`; the thread row in `state_5.sqlite` reported `gpt-6-luna`/`xhigh`. The worker used isolated worktree `D:/sources/worktrees/amaterasu-audio-open-selected-clippy-xhigh`, branch `codex/luna04-audio-open-selected-clippy-xhigh`, based cleanly on `d71ba1db`.
- Changed path: `modules/audio/src/platform/windows.rs` only. The private `OpenSelectedOptions` groups desired channel layout, preferred valid bits, capture-only routing, and Console-default-follow policy. `open_selected` now takes five arguments instead of eight; both callers populate the options object. No behavior or feature changes were made. Lead review confirmed exact path ownership and both plan trailers.
- Worker and lane `rustfmt --edition 2024 --check modules/audio/src/platform/windows.rs` and `git diff --check` passed. Cargo tests/build/Clippy were not run in the lightweight worktrees.
- The coordinator reported main commit `f9ce0a86` passed 49 Windows-target audio tests with one hardware test ignored; strict Clippy found only the original `clippy::too_many_arguments` on `open_selected`. Coordinator must rerun tests and strict Clippy after landing this correction. No Windows audio hardware qualification was run.

## Previous bounded slice: follow Windows Console default

- Lane code candidate: `c694ce65e97b378d57be7ff9272f7387a1db797c` (`fix(audio): follow the Windows Console default`); isolated worker source commit: `6b8309d271b710964516c5eff990b5906f4d07e0`; worker base: `bff3338ab61906f89b4e8d97afbe510dee7db2a0`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`. Final lane SHA after the handoff update is recorded by the handoff commit and outbox `015-audio-console-default-runtime.json`.
- Stable ID touched: `global:audio_sink`. It remains pending; `global:audio_sink_capture_only` and `global:virtual_sink` also remain pending.
- Verified worker session `01a0d5cf-9972-7e21-b4b0-6e50485f0d7e` (`gpt-6-luna`/`xhigh`; parent thread `01a0d516-8713-7b83-87f1-d35c0828ae10`) worked in isolated worktree `D:/sources/worktrees/amaterasu-audio-default-follow-xhigh`, branch `codex/luna04-audio-default-follow-xhigh`. The worker confirmed the worktree was clean at the lane base before edits.
- Changed paths: `modules/audio/README.md`, `modules/audio/src/platform/mod.rs`, `modules/audio/src/platform/windows.rs`, and `modules/audio/src/worker.rs`. The worker commit was reviewed and cherry-picked into the lane; it contains the required plan trailers.
- With no explicit or automatic virtual sink, empty and `default` regular endpoint selectors now initially resolve to the Windows Console render endpoint and follow Console endpoint changes. The audio worker checks every 100 ms, immediately attempts capture reacquisition when the endpoint changes or the query fails, and uses the existing cancellable five-second delay only after a failed reopen. These captures do not change Windows default roles. Explicit endpoint IDs and explicit/automatic virtual sink selections remain pinned. Format validation, packet sequence/timestamp continuity, and first-recovered-packet discontinuity behavior remain in place.
- The `amaterasu.audio/1` payload shape and `AudioCapture` API are unchanged. Endpoint reporting remains the initially selected endpoint snapshot so existing stream-session shared-lease identity remains stable; lease metadata is not updated after a followed default change. This limitation is documented in the audio README.
- Cross-lane contract proposal was sent in outbox `014-audio-default-endpoint-contract.json` to the coordinator and L02/L05/L06 before implementation. The coordinator approved these selector semantics in inbox `coordinator-audio-default-endpoint-decision.json` and requested each consumer lane acknowledge or identify a concrete conflict. No foreign-owned paths or shared schemas were edited; record the pending L02/L05/L06 acknowledgements before changing their consumer paths.
- Added selector-policy and fake-worker tests for default aliases, virtual-selection exclusion, changed-endpoint reacquisition, packet-clock continuity, discontinuity, and explicit endpoint pinning. Tests/build/Clippy were not run in either lightweight worktree. `rustfmt --edition 2024 --check modules/audio/src/platform/mod.rs modules/audio/src/platform/windows.rs modules/audio/src/worker.rs` and `git diff --check` passed in the worker and lane.
- Windows device-change/hardware qualification was not run. The implementation uses periodic COM default-endpoint queries; default switching on a real Windows render device remains unqualified. Non-Windows audio capture remains unsupported. Persistence/default migration, scoped client/application resolution, management edit authority, and full audio streaming parity remain incomplete; do not close any stable ID.

## Previous bounded slice: virtual sink policy Clippy correction

- Original lane code candidate: `1d8bb712fb2e538d29a5ac5ee55d739caffa84a4` (`fix(audio): normalize virtual sink capture policy`); worker source commit: `9309259000e87a2cbc88703179abf75bf8310e42`. The coordinator landed it on main as `3914de70f487ec035fefcf46c5f5745655348dd6`.
- Clippy correction lane commit: `83ace03562570ffb18633c864b5227aa09f6c026` (`refactor(audio): fold virtual sink check into open match`); worker source commit: `7c5cb8f717062d88832d92ae1758a3f2a2a27e8b`; correction base: `e527a5e3cb95da5ed45d1e94e3fe722ea0950665`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable IDs in scope: `global:audio_sink_capture_only` and `global:virtual_sink`. Both remain pending full parity review.
- The verified worker session `01a0d596-d04d-7801-b897-d9e289125eb8` (`gpt-6-luna`/`xhigh`) worked in isolated worktree `D:/sources/worktrees/amaterasu-audio-policy-next-xhigh`, branch `codex/luna04-audio-policy-next-xhigh`. It owned only `modules/audio/src/lib.rs` and new `modules/audio/src/open_policy.rs`.
- The correction worker session `01a0d516-8713-7b83-87f1-d35c0828ae10` / thread `01a0d5b0-2eee-7822-a1b2-290f19c48097` was independently verified in `state_5.sqlite` as `gpt-6-luna`/`xhigh` before edit authorization. Its isolated worktree was `D:/sources/worktrees/amaterasu-open-policy-clippy-xhigh`, branch `codex/luna04-open-policy-clippy-xhigh`; it owned only `modules/audio/src/open_policy.rs`.
- The public audio request ingress now clears `capture_sink_without_routing` when an explicit virtual sink selector is present, including the empty selector used for the default Steam virtual endpoint. It preserves every other operation field and leaves non-Open operations unchanged. This makes the direct `amaterasu.audio/1` service path comply with the pinned rule that capture-only routing applies only without a configured virtual sink.
- The focused Clippy fix matches `virtual_sink: Some(_)` directly in the `Operation::Open` pattern, replacing the nested `is_some()` branch. `Some("")` remains a present selector and still clears capture-only; `None` retains its capture-only value; non-Open operations remain unchanged. Lead review confirmed the worker commit changes only the authorized path and contains both required plan trailers.
- Inspection found the regular NVHTTP-to-stream-session path already sets capture-only false when a virtual selector is configured and rejects an invalid virtual-sink/capture-only combination before calling audio. This change therefore covers direct audio service requests; it does not alter normal route construction or add configuration persistence/resolution.
- Coordinator validation of main commit `3914de70f487ec035fefcf46c5f5745655348dd6` passed Windows-target tests (`46 passed, 0 failed, 1 ignored live-endpoint test`) and reported strict Clippy's only finding as `clippy::collapsible_if` in `open_policy.rs:4`. In the lightweight lane, `rustfmt --edition 2024 --check modules/audio/src/open_policy.rs` and `git diff --check e527a5e3cb95da5ed45d1e94e3fe722ea0950665 HEAD` passed after the correction. Cargo tests, build, and Clippy were not run here; coordinator rerun is pending.
- The four policy tests added with the original candidate cover nonempty and empty virtual selectors, preserving capture-only with no selector, and preserving non-Open operations. They were not rerun in this lane. No Windows endpoint, virtual audio driver, or hardware qualification was performed. Recovery paths and other lanes were not modified; no main edit or direct-to-main cherry-pick was performed by this lane.

## Previous bounded slice: recovery Clippy correction

- Lane code candidate: `ecf2a15c7782deb15b29effd6e39ad3149156a06` (`fix(audio): group capture recovery expectations`); worker source commit: `18e6da37cf2ac7739795913ee67cba3373d88c63`.
- Base SHA: `f23da668410c8ca1dce7d9fb38b1415dc0cfeb37`; branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Coordinator validation of the landed recovery source passed `cargo fmt --check --package amaterasu-audio` and Windows-target tests (`42 passed, 0 failed, 1 ignored`). Strict Clippy failed only for `clippy::too_many_arguments` on `run_capture_loop` at `worker.rs:230`; the coordinator requested a focused correction before the next parity slice.
- The verified XHigh worker session `01a0d596-d04d-7801-b897-d9e289125eb8` (`gpt-6-luna`/`xhigh`) changed only `modules/audio/src/worker.rs`. `RecoveryPolicy` groups the expected endpoint, expected PCM format, and retry delay; the worker entry and three unit-test call sites were updated without changing recovery behavior.
- Lead review and cherry-pick completed. `rustfmt --edition 2024 --check modules/audio/src/worker.rs` and `git diff --check f23da668410c8ca1dce7d9fb38b1415dc0cfeb37 HEAD` passed in the lightweight lane. Cargo tests/build/Clippy were not run here; the coordinator must rerun them on main.
- The Windows endpoint-dependent test remains ignored without a controlled endpoint. Device loss/re-enumeration and route lease release/reacquisition still need hardware qualification. No main edits or direct-to-main cherry-pick were performed by this lane.

## Previous bounded slice: Windows capture recovery

- Lane candidate: `0c9e1fb9be6759827693ed93a11c6e678a479cfd` (`fix(audio): recover Windows capture after device loss`).
- Worker source commit: `f6c4c8dd633c9d5924415e08dceaea52016c012e`; lane base: `87caa50f886965173a3cba09eedd5746a6daedc7`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable IDs advanced locally: `global:audio_sink`, `global:virtual_sink`; both remain pending full parity review.
- Worker session `01a0d56e-93d9-74b3-8c74-02774f028536` was verified before edits as `gpt-6-luna`/`xhigh` from active session attribution metadata.
- Changed paths: `modules/audio/README.md`, `modules/audio/src/controller.rs`, `modules/audio/src/controller/tests.rs`, `modules/audio/src/model.rs`, `modules/audio/src/packet.rs`, `modules/audio/src/platform/mod.rs`, `modules/audio/src/platform/windows.rs`, and `modules/audio/src/worker.rs`.

After an initialized Windows capture reports `device_lost`, the worker releases it and retries reopening the same selected endpoint and effective capture configuration every five seconds until success or shutdown. Acquisition failures continue retrying. A changed endpoint or PCM format fails terminally so the existing `AudioCapture` format contract stays fixed. Retry and idle waits are interrupted by worker shutdown. A stream-level timeline keeps packet sequence and timestamps monotonic across capture resets and marks the first recovered packet discontinuous; status reports `recovering` until audio resumes.

`rustfmt --edition 2024 --check` and `git diff --check 87caa50f886965173a3cba09eedd5746a6daedc7 HEAD` passed in the worker/lane checkouts. Cargo tests, builds, and Clippy were not run in these lightweight worktrees; coordinator validation is pending. Tests were added for acquisition retries, shutdown cancellation, timeline continuity, format mismatch, and recovering status, but were not executed. No Windows hardware/device-removal qualification was performed. Non-Windows audio capture remains unsupported.

This slice does not complete `global:audio_sink` or `global:virtual_sink`. Opus encoding, configured speaker remapping, resampling, continuous silent audio, process-specific loopback, microphone capture, Linux/macOS capture, automatic endpoint switching, and full streaming audio parity remain unimplemented. Recovery stays pinned to the originally selected endpoint.

The coordinator owns direct lane-commit cherry-picks into `main`; no integration branch is used. No main cherry-pick was performed in this lane.

## Earlier bounded slice: DS4 lease snapshot correction

- Candidate source commit: `431acd70a632f6960b22be18a915cc49107c215a` (cherry-picked from worker candidate `bd03e85c2b26fb62dbe08e73a9a2c813e28092cb`).
- Follow-up test expectation commit: `2f8775d230628f272c8290f802f6e193ab2b7de7` updates the v9 absent-document bootstrap default assertion discovered by the full package suite.
- Base SHA: `3adbd51f434c2956dfedb6301e0fd9d008b4ce0e`.
- Branch: `config-parity/audio-input`; source baseline: `f220bcaa4a9d4c28e410a877b0809c14d08bb88b`.
- Stable ID: `global:ds4_back_as_touchpad_click` (global and sparse per-application/per-client override).
- Worker: session `01a0d559-d1e8-7fd2-aa27-6a2b42fc7fa6`, verified `gpt-6-luna`/`xhigh` before edits; task was lease snapshot/retry semantics in the input controller.
- Changed paths: `modules/input/src/controller.rs`, `modules/input/src/controller/tests.rs`, and `modules/input/src/product_settings/tests.rs`.

The lease now stores the effective DS4 back mapping computed at acquisition. The original sparse request stays as `Option<bool>` for operation replay and conflict checks: an explicit false remains `Some(false)`, and the v9 default continues to pass through as `None`. A global settings change cannot alter the active lease; release restores the latest global value. The backend setting only affects the DS4 report mapping.

Validation: `rustfmt --edition 2024 --check` passed on the changed Rust files. `cargo test -p amaterasu-input --target x86_64-pc-windows-msvc ds4_back` passed all 5 matching tests. The full package command `cargo test -p amaterasu-input --target x86_64-pc-windows-msvc` passed all 113 tests after updating the stale v9 bootstrap expectation. It reports the existing `windows_gamepad.rs::start_with` dead-code warning. The installed Windows MSVC target verifies cross-target compilation and pure unit tests; no ViGEmBus device/runtime qualification was performed.

The coordinator's landing policy is direct lane-commit cherry-picks into `main`; no integration branch is used. The coordinator owns those cherry-picks. This correction leaves `global:ds4_back_as_touchpad_click` pending until management/client/app resolution and Windows device qualification are reviewed.

### Expanded acquire and retry contract acknowledgement

- `Lease.ds4_back_as_touchpad_click` snapshots the effective value at acquisition using the explicit request when present or the current durable ProductSettings v9 global default otherwise. The lease stores the sparse request separately: `Some(false)` remains false, while `None` falls through to the durable default. Status reads the lease snapshot, and cleanup restores the latest global policy after releasing the lease.
- Input acquire callers send `Some(value)` only for an explicit client/application override; absent overrides send `None`. The session retry identity freezes that optional value so retries preserve the original scope decision. The mapping applies only to DS4; X360 behavior is unchanged.
- The source currently checked on local `main` contains the lease field and snapshot code in commit `3d1fa5ac09ff1411434767679c238bc7459127cd`; the earlier missing-field compile error is absent from that checked tree. No Cargo validation was run in this lightweight lane, so coordinator package validation remains authoritative.

## Initial DS4 profile and settings wire-up

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

Record acknowledgements to the coordinator's default-endpoint decision as they arrive, without editing L02/L05/L06 paths. Then inspect the remaining runtime gaps for `global:audio_sink_capture_only` or `global:virtual_sink` and choose a bounded audio-owned change that respects the current L02 persistence and launch contract. Persistence, scoped resolution, management edit authority, full audio streaming behavior, and Windows device qualification remain open; keep all three audio IDs pending until the coordinator reviews full parity and platform evidence.

## Earlier recovery attempt (stopped before code edits)

- Recovery attempt base SHA: `4af446961b46a516305d49dc53376d7cf8a2d35e` (the DS4 handoff commit); no code was changed in that attempt.
- Blocker-record commit: `554ac24` (`docs(audio): record recovery worker effort blocker`).
- IDs and scope: `global:audio_sink`, `global:virtual_sink`.
- Proposed owned paths: `modules/audio/src/worker.rs`, `modules/audio/src/platform/windows.rs`, `modules/audio/src/platform/mod.rs`, `modules/audio/src/packet.rs`, `modules/audio/src/model.rs`, `modules/audio/src/controller.rs`, and `modules/audio/README.md`.
- Tests: none run or added for capture recovery in that attempt; Windows capture/device qualification is unavailable here.
- Worker request: `recovery_timeline` was requested as `gpt-6-luna`/`xhigh`, with exclusive ownership of `modules/audio/src/packet.rs` and a requirement to confirm settings before edits. Its runtime attribution reported session `01a0d516-8713-7b83-87f1-d35c0828ae10` as `gpt-6-luna`/`max`, so it made no changes and this recovery attempt stopped before code edits. The DS4 lease correction separately confirmed a native XHigh worker can start, but audio recovery was not assigned in that slice.
- That attempt was superseded by the current verified XHigh worker slice above. It did not modify the current implementation.
