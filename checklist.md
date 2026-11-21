# Lifecycle checklist

All items are open. The original 400 source audits are complete: 16 Luna Medium coordinators and 384 Luna Medium investigators. The additional coverage-confirmation loop completed **20 additional Luna Medium reviewers in five iterative batches**.

**Required invariant:** one designated, replaceable provider decides whole-stream criticality and failure. Lower layers propagate facts/errors. Noncritical conditions become warnings with a valid effective plan. Explicit user stop and resource-local safety/rollback are distinct. Ended cleanup cannot monopolize unrelated admission or lose its exact durable obligations.

This is a source audit of commit `32cd937351f7b99178ab55a00c4b1c3ae68f59d3` plus the existing uncommitted work. No fixes, builds, tests, installation or client playback were performed during the audit. Conditional items below are verification work, not confirmed default-client failures.

## Central failure authority — one systemic problem

- [ ] **C01 — Add the one designated, replaceable failure-decision provider.**
  Define a versioned SDK decision contract selected by trusted startup composition. Providers propagate facts, requiredness, effect/ownership evidence, stream generation and operation identity. Only the handler may authorize a whole-stream critical failure, with a stable reason. Make downstream transitions consume that decision or an explicit normal-stop intent. The event topic and store remain diagnostic; event delivery and store health must never become admission gates.
  **Acceptance:** Replace the handler without changing consumers: warning continues through a valid effective plan; critical authorizes the exact generation to fail; missing/late/duplicate decisions cannot fabricate success, reopen an ended generation, or stop a successor.
  **Evidence:** [src/pipeline/workflow.rs:134](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/workflow.rs:134); [src/coordinator/providers.rs:2094](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:2094); [release/src/profiles.rs:54](/home/chasep/sources/amaterasu/crates/release/src/profiles.rs:54).

- [ ] **C02 — Route stream-session startup failure decisions through that handler.**
  Remove independent whole-stream classification from generic Job::fail and startup display/video/audio/control/transport readiness paths. Include failed display Apply, virtual-display preflight/status/readiness, and the selected source-preparation hook in the site/test matrix. A warning requires a verified compatible plan and preserved explicit policy. Component-local errors and failed-attempt rollback remain legitimate. Keep required format, authorization and uncertain native effects intact; choose fallback only when hard requirements remain satisfied.
  **Acceptance:** Inject the same startup fact with handler warning and critical responses. A warning must not set global Failed/Stopping or tear down a usable plan; critical must authorize the failure once.
  **Evidence:** [src/pipeline/workflow.rs:134](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/workflow.rs:134); [src/pipeline/lifecycle.rs:1429](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:1429); [src/pipeline/lifecycle.rs:1121](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:1121); [src/pipeline/lifecycle.rs:1465](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:1465); [src/pipeline/lifecycle.rs:1492](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:1492).

- [ ] **C03 — Route active media, input and feedback failures through the handler.**
  Cover media-pump errors, audio recovery, input lease expiry, ambiguous batches, feedback polling, rumble sends and active ForceIdr/keyframe refusal or timeout. Do not assume a refused keyframe leaves the output decodable; central policy must consider decode continuity. Preserve immediate resource-local containment and no-replay fences. Required-input or genuinely dead transport failures may be critical; an optional-feedback label is not proof that every native error is harmless.
  **Acceptance:** Keep valid media alive for a handler-authorized warning; retain uncertain input/haptic obligations. Required authentication, stale lease and hard media failures still take the handler-authorized critical path.
  **Evidence:** [stream-session/src/pipeline.rs:1205](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline.rs:1205); [src/pipeline/workflow.rs:135](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/workflow.rs:135); [stream-session/src/pipeline.rs:1317](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline.rs:1317); [src/pipeline/input.rs:527](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/input.rs:527); [stream-session/src/pipeline.rs:1183](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline.rs:1183); [src/pipeline/lifecycle.rs:1184](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:1184); [src/pipeline/input.rs:528](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/input.rs:528).

- [ ] **C04 — Centralize trusted NVHTTP launch/resume and application-group failure decisions.**
  Route trusted refusals, presentation/source binding failures, launch replies and application-health monitoring through central adjudication before global cleanup. Scope the decision to the affected client generation or shared application group. Include a valid nonterminal `stopping` MonitorApplication reply in the test matrix: propagate an already authorized stop intent, otherwise obtain central policy before a new group-failure decision. The saved audit does not establish whether providers emit that status only after an authorized stop. Keep pre-authentication syntax/permission refusals as request errors and preserve explicit Quit/Cancel intent.
  **Acceptance:** Warning does not invoke request_cleanup or end healthy joined clients. Critical applies to the authorized target only. Pre-authentication parse/permission denials remain request-scoped, do not alter or terminate an existing stream, and do not require whole-stream failure adjudication. Request-scoped refusal reporting may remain available without granting untrusted callers stream control or making telemetry a gate. Ordinary application completion and explicit cancellation remain normal lifecycle outcomes.
  **Evidence:** [nvhttp/src/launch.rs:1244](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch.rs:1244); [src/launch/workflow.rs:4137](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/workflow.rs:4137); [src/pipeline/stream_issues.rs:54](/home/chasep/sources/amaterasu/modules/launch/src/pipeline/stream_issues.rs:54); [src/launch/workflow.rs:1761](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/workflow.rs:1761).

- [ ] **C05 — Make RTSP consume central decisions and preserve normal-stop intent.**
  Audit ticket refusal, arm/start, network-loss and Stop scheduling sites. A valid already-terminal upstream status can be propagation of an existing decision or normal stop; do not require a new independent handler at every layer. Carry the original decision/intent instead of treating an arbitrary failed string as fresh authorization.
  **Acceptance:** Propagate one decision across the layers without duplicate adjudication. Cover explicit TEARDOWN, local rollback, terminal status and lost observations separately.
  **Evidence:** [src/controller/workflow.rs:554](/home/chasep/sources/amaterasu/modules/rtsp/src/controller/workflow.rs:554).

- [ ] **C06 — Centralize coordinator preparation failure and failed-start cleanup policy.**
  PrepareDriver errors currently reach fail_work and persist Ending/Quiescing before central adjudication. Include capture_invalidated/resource_invalidated in coordinator operations.rs:614–659 as facts requiring recovery/failure policy, distinct from explicit pause_stream/end_session. Preserve immediate resource-local containment and fencing; the handler decides any whole-stream failure/release. Separate local rollback from that global decision and retain exact cleanup evidence.
  **Acceptance:** A provider rejection or recoverable invalidation with an approved safe plan can receive a warning; required output/ownership failure receives critical. Explicit pause/end remains normal intent. No fabricated Prepared or retirement receipt may make the test pass.
  **Evidence:** [src/coordinator/providers.rs:2071](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:2071).
  **Additional site:** [capture/resource invalidation](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/operations.rs:614).

- [ ] **C07 — Separate provider facts from authoritative global critical reporting.**
  Lower publishers currently construct CriticalFailure themselves and the reducer records it. Make the handler the authority for global disposition, while keeping originating provider code, stable reason, authenticated publisher, causal chain and source location. Preserve compatible legacy fact reporting without turning telemetry failure into stream failure. Native modules remain trusted code; API grants are not a native-code sandbox.
  **Acceptance:** A lower-layer fact cannot independently mark the global stream failed. Diagnostic loss is observable, yet launch/continuation does not depend on optional reporting delivery.
  **Evidence:** [module-sdk/src/stream_lifecycle.rs:230](/home/chasep/sources/amaterasu/crates/module-sdk/src/stream_lifecycle.rs:230).

## Noncritical startup and continuation blockers

- [ ] **B01 — Wait or retry inside RTSP startup reconciliation grace.**
  A valid open during the ten-second reconciliation window is immediately refused and NVHTTP treats it as terminal. Hold/retry the same operation within its fixed remaining budget; preserve real resource conflicts and retirement evidence.
  **Acceptance:** Hold reconciliation pending, finish within the grace, and let the same launch reach readiness without a second client attempt.
  **Evidence:** [src/controller/actions.rs:144](/home/chasep/sources/amaterasu/modules/rtsp/src/controller/actions.rs:144).

- [ ] **B02 — Keep optional extension probes off the critical prepare deadline.**
  A pending optional probe can exhaust the whole prepare budget before its fallback runs. Give optional observation its own bounded budget or skip it when there is insufficient startup budget, while retaining exact independent settlement.
  **Acceptance:** Rejected, absent, malformed, lost and indefinitely pending optional probes reach fallback before the required startup deadline.
  **Evidence:** [src/pipeline/lifecycle.rs:123](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/lifecycle.rs:123).

- [ ] **B03 — Skip unapproved optional leaves before authority allocation.**
  Optional approvals can be considered complete while work_selections excludes that provider and authority.allocate then errors. Remove the independent optional leaf consistently and record a warning; required dependents still need their outputs.
  **Acceptance:** A required owner plus an independent optional provider with no approval completes startup; the required-dependent counterexample fails explicitly and safely.
  **Evidence:** [src/coordinator/providers.rs:1401](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:1401).

- [ ] **B04 — Preserve optionality in the public processing contract and adapter.**
  The public lifecycle participant projection loses optionality and adapter conversion hardcodes required behavior. Version the contract compatibly and carry trusted optional/dependency policy through every adapter.
  **Acceptance:** The public SDK composition path preserves an independent optional provider and survives its absent approval without spending the caller deadline.
  **Evidence:** [session-lifecycle/src/adapter.rs:170](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/adapter.rs:170).

- [ ] **B05 — Make optional skips consistent with finalization and control proofs.**
  Skipping currently advances a cursor while the graph still requires the provider Prepared, producing finalize_graph_incomplete. Represent a genuine skip/effective graph change; do not invent provider Prepared outputs or cleanup receipts.
  **Acceptance:** An optional no-effect plan rejection completes finalization with a warning; a required dependent cannot dispatch using missing or fabricated inputs.
  **Evidence:** [src/coordinator/providers.rs:2076](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:2076); [src/coordinator/composition_prepare.rs:57](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/composition_prepare.rs:57).

- [ ] **B06 — Demote proven no-effect optional frame-limiter callback rejection.**
  Empty Linux Proton tool discovery and proven pre-effect queue/capacity failures reach RecoveryRequired and fail_work even for an optional limiter. Preserve a typed no-effect fact for central adjudication and a validated no-limiter plan.
  **Acceptance:** Empty discovery warns and streams without a limiter; uncertain or effectful callbacks retain their exact ownership/recovery obligations.
  **Evidence:** [src/coordinator/providers.rs:2406](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:2406).

- [ ] **B07 — Keep uncertain optional history reservation out of resume failure.**
  ReserveHistory uncertainty before RTSP can fail a resumed launch. First establish exact settlement or implement durable generation-scoped identity isolation, then allow media to continue without optional history. A lost reply is not proof of no reservation. Until safe isolation exists, retain the exact operation and its safety fence; simply retaining a global history-dependent resume rejection is not the finished fix. The saved audits disagree about immediate safe demotion, so this is an identity-isolation design task, not authorization to bypass an unresolved mutation.
  **Acceptance:** Prove the late reserve/End race against a reused activity identity is safe before removing the fence. A successor with an independently isolated history identity can resume while old settlement remains pending, and old cleanup cannot end its activity. Confirmed rejection/reserved:false can use the existing warning path.
  **Evidence:** [src/launch/workflow.rs:810](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/workflow.rs:810).

- [ ] **B08 — Skip audio packetizer construction for video-only sessions.**
  Open validation skips disabled audio, but packetizers() still constructs AudioPacketizer and can reject unused values, including wire_audio_bytes=0 on a video-only stream-session path. Default NVHTTP audio-on settings were not shown affected.
  **Acceptance:** A valid video-only consumer reaches media readiness with unused audio values; audio-enabled wire validation remains strict.
  **Evidence:** [media-transport/src/stream.rs:160](/home/chasep/sources/amaterasu/modules/media-transport/src/stream.rs:160); [media-transport/src/stream.rs:906](/home/chasep/sources/amaterasu/modules/media-transport/src/stream.rs:906).

- [ ] **B09 — Handle poisoned advisory foreground policy without an automatic media stop.**
  On Linux with linux-nvidia, a prior panic can poison the foreground-policy mutex and become MEDIA_BACKEND_ERROR, ultimately stopping media. Use a validated safe unavailable/disabled-conversion policy when it preserves the negotiated output; send any genuine hard processing failure to the handler. Ordinary WouldBlock is already benign.
  **Acceptance:** Poison the advisory policy mutex and preserve exact-format playback or obtain an explicit central critical decision when no safe output exists. Do not swallow unrelated native processing failures.
  **Evidence:** [rtx-hdr/src/linux.rs:440](/home/chasep/sources/amaterasu/modules/rtx-hdr/src/linux.rs:440).

- [ ] **B10 — Adjudicate active rumble-zero deadlines as optional-effect policy.**
  A 200 ms zero-delivery deadline can force Failed/Stopping while an active stream otherwise runs. Keep remote stop uncertainty and exact bounded retry/containment separate from media policy; do not falsely assert haptics retired. A dead required control channel may still justify central criticality.
  **Acceptance:** Expire an active rumble-zero deadline: warning can preserve healthy media with the obligation retained; a required transport failure still follows a central critical decision.
  **Evidence:** [stream-session/src/pipeline.rs:1144](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline.rs:1144).

## Admission, cleanup ownership and bounded retention

- [ ] **R01 — Separate old cleanup/recovery jobs from successor request polling.**
  One retained job returns before Poll, so an old cleanup can hold the only request slot while media still pumps. Use independent bounded lanes and ordered/CAS journal writes, without stale whole-document overwrites or resettable deadlines.
  **Acceptance:** Keep A cleanup pending; B is read, prepared and reaches actual client playback within its original finite budget. Late A writes/results cannot overwrite B.
  **Evidence:** [stream-session/src/pipeline.rs:558](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline.rs:558).

- [ ] **R02 — Retain or durably transfer unresolved cleanup identity at the 32-entry journal bound.**
  Ended unconfirmed entries can be forgotten. Exact virtual-display caller reference identity can disappear while the provider correctly retains its native reference and recovery fence. Partition retirement work or transfer exact ownership durably while unrelated admission proceeds; rejecting every new stream at 32 is not the completed fix.
  **Acceptance:** Saturate the journal, restart, recover every unresolved identity, and admit an unrelated successor. Verify old native references retire through real evidence, never journal edits.
  **Evidence:** [src/pipeline/admission.rs:31](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/admission.rs:31); [src/pipeline/admission.rs:24](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/admission.rs:24).

- [ ] **R03 — Preserve retry orchestration across NVHTTP and launch-record eviction.**
  Capacity eviction can discard settled-but-unconfirmed application cleanup retries at both layers. Trace existing downstream ownership and durably hand off exact retry identity before eviction. The audit does not prove a particular native process remains active merely because a record was evicted.
  **Acceptance:** Pressure both tables while cleanup stays unconfirmed: successor admission works and the exact predecessor retry remains independently observable and retryable.
  **Evidence:** [src/launch/cleanup_recovery.rs:263](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/cleanup_recovery.rs:263); [launch/src/records.rs:455](/home/chasep/sources/amaterasu/modules/launch/src/records.rs:455).

- [ ] **R04 — Stop ended unresolved sessions consuming the coordinator active quota.**
  The 128-session check counts retained ended records before domain planning. Partition durable retirement evidence from active admission and keep real conflicting resource fences.
  **Acceptance:** 128 ended owners pinned on domain A do not reject a valid independent-domain B; all A obligations remain durable.
  **Evidence:** [src/coordinator/operations.rs:304](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/operations.rs:304).

- [ ] **R05 — Replace permanent admission exhaustion at 8,192 replay tombstones.**
  Use a durable safe checkpoint/partition/generation scheme so old closed operations do not permanently block prepare/resume. Preserve fail-closed replay checks until the replacement is proven; never simply delete fences.
  **Acceptance:** Cross the old bound, restart and admit fresh work while replayed old operations remain rejected; checkpoint failure preserves safety.
  **Evidence:** [session-lifecycle/src/coordinator.rs:1474](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator.rs:1474).

- [ ] **R06 — Bound NVHTTP replay fingerprints on every insertion path.**
  Capacity eviction inserts retired keys beyond MAX_RETIRED_STREAM_KEYS=32,768 even though other paths check the limit. Use one safe bounded/durable policy without creating a new permanent unrelated-admission barrier.
  **Acceptance:** Repeated eviction cannot grow the set beyond policy; old keys cannot regain authority, and safely independent work remains admissible.
  **Evidence:** [src/launch/cleanup_recovery.rs:282](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/cleanup_recovery.rs:282).

## Diagnostics and failure attribution

- [ ] **D01 — Mark lifecycle feed continuity loss across reconnect.**
  Dropping a failed Subscription loses the missing-handle gap sentinel; a fresh subscription can leave audit_gap=false despite missed reports. Preserve loss state across reconnect and surface incompleteness.
  **Acceptance:** Established feed -> overflow/read error -> failed renewal -> fresh subscribe marks affected history incomplete while streaming continues.
  **Evidence:** [src/stream_lifecycle/feed.rs:63](/home/chasep/sources/amaterasu/crates/module-sdk/src/stream_lifecycle/feed.rs:63).

- [ ] **D02 — Commit the non-confirming pause-snapshot fallback.**
  The valid-but-non-confirming VerifyPaused result can log pause_unconfirmed and retain the app without a lifecycle demotion. Record the stable warning in the common store.
  **Acceptance:** A ready snapshot through the pause deadline keeps the app controllable and records the exact demotion once.
  **Evidence:** [src/launch/workflow.rs:1935](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/workflow.rs:1935).

- [ ] **D03 — Keep requested TEARDOWN disconnect distinct from playback failure.**
  A disconnect after explicit TEARDOWN can introduce a critical network_disconnected root cause. Preserve normal end intent and report any canceled Stop/native cleanup uncertainty separately.
  **Acceptance:** Disconnect during TEARDOWN adds no false playback failure; confirmed and unconfirmed Stop outcomes retain their correct evidence.
  **Evidence:** [src/controller/network.rs:173](/home/chasep/sources/amaterasu/modules/rtsp/src/controller/network.rs:173).

- [ ] **D04 — Carry extension ANNOUNCE adjustments to host alerts and exact-stream history.**
  Extension acceptance drops bounded parser adjustments that the standard path retains. Preserve profile opacity and negotiated format while reporting the actual fallback.
  **Acceptance:** An ignored optional extension hint produces a bounded host warning and exact-activity history entry without changing successful media negotiation.
  **Evidence:** [src/controller/record.rs:114](/home/chasep/sources/amaterasu/modules/rtsp/src/controller/record.rs:114).

- [ ] **D05 — Conservatively project malformed optional HDR catalog metadata.**
  A valid generic properties object with wrong-type hdr_supported currently aborts the entire catalog. Project that row as HDR unsupported with a bounded warning; required identity/visibility/security validation stays strict. This is a catalog response defect, not demonstrated RTSP rejection.
  **Acceptance:** String/object/null hints preserve the useful catalog with conservative HDR capability; valid false still narrows host capability.
  **Evidence:** [wire/presentation/applications.rs:477](/home/chasep/sources/amaterasu/modules/nvhttp/src/wire/presentation/applications.rs:477).

- [ ] **D06 — Scope rejected resume diagnostics to the attempted generation.**
  reject_resume can record a critical root cause while retaining the root Paused/Live. Distinguish a failed attempt from the still-owned application/other clients, and let the central handler decide disposition. Genuine hard refusal of that attempt remains possible.
  **Acceptance:** A refused resume cannot falsely mark the retained root or another live client failed; the attempt retains accurate request-level diagnostics.
  **Evidence:** [src/launch/workflow.rs:4162](/home/chasep/sources/amaterasu/modules/nvhttp/src/launch/workflow.rs:4162).

- [ ] **D07 — Project contract-valid nonnumeric current-game references without discarding useful presentation output.**
  Saved scope02 worker06/13/20 reports identify a distinct valid-token case omitted by the coordinator: a bounded application reference such as `desktop-main` passes the generic contract but fails NVHTTP's numeric Moonlight projection. Use the established catalog mapping or a verified safe no-current-game representation with a bounded warning. Verify zero versus omission and preserve accurate busy/ownership state; malformed identity and UUID replies remain separate rejected cases.
  **Acceptance:** A valid nonnumeric reference preserves useful server-info/catalog output and accurate admission state. Test the client's actual wire semantics before selecting a fallback.
  **Evidence:** [worker13](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/02_nvhttp_admission/worker13.json), [worker20](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/02_nvhttp_admission/worker20.json), [coverage review](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/verify02_scope02.json).

## Contract and composition gaps

- [ ] **G01 — Support independent v1 providers alongside v2 composition without fabricated v2 output.**
  The homogeneous-v2 profile gate is intentional and currently required for its proof path, but leaves a documented independent-provider compatibility gap. Add a versioned adapter/composition path or an explicitly limited profile; preserve existing safety guards until equivalent proofs exist.
  **Acceptance:** An unchanged consumer works with an independent v1 participant and a v2 graph; genuinely required v2 inputs remain enforced.
  **Evidence:** [session-lifecycle/src/adapter.rs:466](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/adapter.rs:466).

## Verification required before prescribing an additional fix

- [ ] **V01 — Resolve safe RTX processing-intent preflight fallback per composition.**
  The preflight error directly reaches fail_work, but a safe exact-format pass-through was not universally demonstrated. Confirm the selected graph can preserve negotiated HDR/range before prescribing warning/bypass; otherwise central criticality is valid.
  **Acceptance:** Test an established exact-format pass-through and a genuinely required HDR conversion case.
  **Evidence:** [src/coordinator/providers.rs:1646](/home/chasep/sources/amaterasu/modules/session-lifecycle/src/coordinator/providers.rs:1646).

- [ ] **V02 — Resolve dormant custom-audio processor validation at the authorized Prepare API.**
  actions.rs:317 rejects missing custom processing even with stream_audio=false. Normal RTSP does not currently construct this custom profile. Establish the supported authorized API contract, then remove an unused processor prerequisite if the request is valid. The separate claim that Record::handles opens an AudioProcessor while disabled was disproved.
  **Acceptance:** Use a valid authorized custom PrepareConfig with audio off; preserve mandatory processing validation when audio is on.
  **Evidence:** [Prepare guard](/home/chasep/sources/amaterasu/modules/stream-session/src/pipeline/actions.rs:317), [audio review](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/12_audio/REPORT.md).

- [ ] **V03 — Define returned-status failure policy without inventing a built-in malformed producer.**
  RTSP/NVHTTP can react immediately to malformed delivered status, but ordinary RTSP timeouts/rejected reads reschedule and no normal malformed producer was proved. Exercise replacement/fault-injection semantics through the central handler; valid terminal, not-found and authorization facts remain distinct.
  **Acceptance:** Malformed observation does not masquerade as proven terminal state; stale-observation bounds and valid required failure cases remain correct.
  **Evidence:** [src/controller/workflow.rs:554](/home/chasep/sources/amaterasu/modules/rtsp/src/controller/workflow.rs:554).

- [ ] **V04 — Qualify bounded outbox overflow observability with a concrete producer.**
  The 64-item outbox drops/counts oldest entries, but the source audit did not establish a normal same-outbox burst above the cap before flush. Confirm exposure and gap accounting before calling this a production defect.
  **Acceptance:** Stress a real producer or retain this as an explicit unconfirmed limit; do not claim event admission proves delivery.
  **Evidence:** [SDK review and bounded-queue limitation](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/01_sdk_store/REPORT.md).

- [ ] **V05 — Resolve lifecycle-store eviction under mixed active, ended and transient occupancy.**
  The original rejection addresses more than 32 active NVHTTP records, but saved reports do not resolve a store holding 31 active keys plus a transient refusal, followed by the 32nd active open. Retained ended keys may also compete. This is an artifact-based unresolved scenario, not a confirmed production defect or a streaming admission problem.
  **Acceptance:** Establish whether the mixed sequence reaches ordinary `open()` at capacity. If so, preserve active diagnostics or explicitly mark loss; if not, record the actual caller guard. Do not gate media on diagnostic capacity.
  **Evidence:** [SDK coverage reconciliation](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/verify01_scope01.json).

- [ ] **V06 — Qualify the separate 4,096 pinned-receipt admission limit.**
  A saved scope08 report raises a medium-confidence limit on coordinator receipts, distinct from 128 sessions and 8,192 tombstones. Establish a supported receipt population and whether the gate can also prevent recovery/retirement operations needed to clear it. No production exhaustion or fresh-playback rejection was demonstrated.
  **Acceptance:** If reachable, keep exact unresolved receipt ownership while allowing the required recovery and independent fresh work through safe partitioning. Do not delete receipts or weaken replay checks to free capacity; otherwise record the proven bound/caller guard and close this candidate.
  **Evidence:** [original receipt report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/08_lifecycle_retirement/worker08_receipts/worker08.json), [coverage review](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/verify08_scope08.json).

- [ ] **V07 — Qualify exact-target PreferHighest fallback after mode discovery fails.**
  A current snapshot may be usable only if the exact selected target and its mode satisfy the fully resolved resolution, refresh, HDR/color, explicit/manual/disabled policy and negotiated media/security contract. Carry that compatibility proof through preparation so a later `available_modes` query does not fail again. Broad current-mode fallback was not proven safe; otherwise keep the fact for C02 adjudication.
  **Acceptance:** Demonstrate a valid same-target fallback and counterexamples for every hard setting before prescribing a warning. Native/client qualification has not been performed.
  **Evidence:** [display coverage reconciliation](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/verify11_scope11.json).

## Acceptance and regression coverage

- [ ] **T01 — Enforce the single-handler invariant across all global failure sites.**
  Use the control-site inventories in all 16 reports. Cover startup, active media, input/feedback, shared application groups and attempted resumes. Global terminal transitions require the central decision or a normal-stop intent; local resource rollback does not.
  **Acceptance:** Replacement-provider tests and structural/API checks catch accidental direct Failed/Stopping transitions.

- [ ] **T02 — Fault-inject every optional branch and preserve hard contracts.**
  Cover unavailable/rejected/malformed/lost/pending/cancelled reads, definite no-effect callback refusal and unknown effect separately. Every true demotion has a stable reason/fallback and host diagnostics independent of optional history.
  **Acceptance:** Exact codec/chroma/HDR/encryption/target and required-input policy remain satisfied; no fake success or retirement.

- [ ] **T03 — Qualify generation isolation through actual client playback.**
  A cleanup remains pending forever; A end acknowledges within its independent bound; B receives and decodes media within the original finite startup budget. Inject late A effects/results, restarts, worker failure and capacity saturation. Required resources need qualified fencing, recovery, containment or replacement.
  **Acceptance:** Run with default and replacement providers and separately qualify native display/audio/input/capture/encoder/network/application backends. Mere ticket acceptance is insufficient.

- [ ] **T04 — Verify reporting integrity, source attribution and bounded delivery gaps.**
  Check operation bindings, exact failed-attempt recovery, origin versus caller reaction, warning persistence, old-generation isolation, feed reconnection, initial subscription timing and bounded queue loss. Initial-window loss remains a qualification case: no reachable producer-before-subscription sequence was established. Preserve safe identifiers and useful requested/effective values without raw secrets/payloads.
  **Acceptance:** No missed-event interval looks complete; optional diagnostics never decide admission, and local queue success is not asserted as remote delivery.

- [ ] **T05 — Validate affected contracts and consumers before landing fixes.**
  Run rustfmt, meaningful focused tests and Clippy for changed Rust modules. Rebuild every affected consumer for SDK/contract/ABI changes and use a complete build when compatibility cannot otherwise be established.
  **Acceptance:** Keep source tests, package activation, native retirement and real-client playback as separate evidence.

## Control-site inventories and original review coverage

C01–C07 address one systemic authority defect, with implementation work at each owning boundary. The scope reports retain exact site inventories and distinguish unauthorized decisions, propagation of an existing terminal state, local rollback and explicit stop. They also preserve rejected hypotheses and qualifications.

| Scope | Original agents | Evidence |
| --- | ---: | --- |
| 01_sdk_store | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/01_sdk_store/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/01_sdk_store/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/01_sdk_store/agent-manifest.json) |
| 02_nvhttp_admission | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/02_nvhttp_admission/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/02_nvhttp_admission/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/02_nvhttp_admission/agent-manifest.json) |
| 03_nvhttp_workflows | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/03_nvhttp_workflows/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/03_nvhttp_workflows/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/03_nvhttp_workflows/agent-manifest.json) |
| 04_rtsp | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/04_rtsp/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/04_rtsp/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/04_rtsp/agent-manifest.json) |
| 05_stream_startup | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/05_stream_startup/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/05_stream_startup/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/05_stream_startup/agent-manifest.json) |
| 06_stream_retirement | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/06_stream_retirement/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/06_stream_retirement/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/06_stream_retirement/agent-manifest.json) |
| 07_lifecycle_planning | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/07_lifecycle_planning/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/07_lifecycle_planning/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/07_lifecycle_planning/agent-manifest.json) |
| 08_lifecycle_retirement | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/08_lifecycle_retirement/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/08_lifecycle_retirement/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/08_lifecycle_retirement/agent-manifest.json) |
| 09_video_pipeline | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/09_video_pipeline/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/09_video_pipeline/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/09_video_pipeline/agent-manifest.json) |
| 10_encode_capture | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/10_encode_capture/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/10_encode_capture/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/10_encode_capture/agent-manifest.json) |
| 11_display | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/11_display/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/11_display/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/11_display/agent-manifest.json) |
| 12_audio | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/12_audio/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/12_audio/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/12_audio/agent-manifest.json) |
| 13_input_transport | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/13_input_transport/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/13_input_transport/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/13_input_transport/agent-manifest.json) |
| 14_application_launch | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/14_application_launch/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/14_application_launch/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/14_application_launch/agent-manifest.json) |
| 15_optional_plugins | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/15_optional_plugins/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/15_optional_plugins/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/15_optional_plugins/agent-manifest.json) |
| 16_host_composition | 25 | [report](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/16_host_composition/REPORT.md) · [findings](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/16_host_composition/coordinator-findings.json) · [24-worker manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/16_host_composition/agent-manifest.json) |

## Coverage confirmation and provenance

- Original source-audit accounting: **400 distinct agents, all complete**, with [identity/model/completion validation](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/accounting.json).
- Additional confirmation: **20 distinct reviewers completed five iterative batches**. The first 16 scope checks account for all **400 original identities** and **672 recorded claim dispositions**; four global checks reviewed accounting, handler authority, optionality/retirement safety, and completeness. Revisions added D07/V05/V06/V07 and refined the pre-auth, history, keyframe, invalidation, display/source-hook and shared-group boundaries. These checks used saved results without repeating the source search. See the [confirmation manifest](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/agent-manifest.json), [applied reconciliations](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/resolutions.json) and [effective claim mapping](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/confirmation/effective-claim-mapping.json).
- [Coordinator-finding mapping](/home/chasep/.codex/visualizations/2026/11/17/01a1e7cf-8961-7dc0-9a30-13d4c0055308/lifecycle-audit/checklist-evidence.json) accounts for all 55 consolidated claims before the additional raw-report checks.
- Scope05 worker22 completed a no-finding rollback review, but its original report file was missing. Its saved result is an explicitly labeled reconstruction from the actual completion summary; no unseen evidence or second review is claimed.
- Rejected examples include the HDR/ten-bit confusion, apparent caller-site loss, the more-than-32-active-record eviction trigger, natural post-activation app exit and automatic activation bypass. Mixed store occupancy remains V05. The activation broker has explicit retirement barriers; normal RTSP status timeouts reschedule.
- Deferred qualification, not established defects: concurrent distinct PulseAudio/PipeWire sink loads while an old load is unresolved (T03), and dependency resolution of audio-disabled profiles (B04/T05). Preserve existing native fences and selected-profile requirements until these cases are supported and qualified.
- Native behavior and end-to-end playback remain unqualified by this source-only audit. Preserve that limitation when closing T03.
