# Stream failure and warning policy

A stream-start rejection must identify a condition that prevents a correct,
authorized stream or leaves an effect whose ownership cannot be established.
An unavailable optional feature, diagnostic, or advisory client hint must not
become an admission requirement. When a usable fallback exists, continue and
record what was unavailable and what was used instead.

Use a critical failure for authentication or authorization, invalid required
wire or encryption parameters, an unsupported selected media format, absence
of the required capture/encoder/transport, conflicting exclusive ownership, or
an unconfirmed native mutation that must be retired before reuse. Do not replace
a selected codec, chroma format, encryption mode, or explicit disabling policy
with a different one merely to produce a successful reply.

For an optional operation, the implementation and its tests should establish:

1. The fallback is safe and preserves the agreed media and security contract.
2. Rejection, an invalid reply, a missing service, and a lost or pending reply
   all reach the fallback without consuming the critical launch deadline.
3. A bounded warning includes a stable code and source, a reason, and useful
   requested/effective values or an OS error when available. Do not put raw
   payloads, paths, command text, or secrets in records.
4. A warning is logged independently of optional plugins. When a compatible
   history provider is present, attach it to the exact stream as a best-effort
   event. Failed diagnostic reporting must not itself reject the stream.
5. An explicit user setting and existing ownership/retirement fences still
   apply. A warning never asserts that an uncertain native effect has retired.

Every failure and every demotion is reported to the launch owner's stream
lifecycle store through `amaterasu_module_sdk::stream_lifecycle`; see
[stream-lifecycle-store.md](stream-lifecycle-store.md). A demotion that is not
reported there is not a demotion this policy accepts.

`amaterasu_module_sdk::stream_warnings` defines the shared scalar record,
validation and size bounds. The owner logs warnings and may query the launch
provider's optional `warnings` action after the launch reply is delivered. It
may submit `record_history_warnings` for the exact reserved RTSP activity.
Older or replacement providers can decline these actions without failing the
stream. The stream-session owner validates caller and activity identity,
deduplicates records, and forwards them as a `stream_warnings` history event
when the session activates. History uses its existing bounded best-effort
queue. Host logs are also bounded and subject to the runtime log service and
its retention policy; recording is not proof that a remote client received an
event.

## Audited paths in this change

| Path | Outcome |
| --- | --- |
| GameStream optional numeric hints and unsupported advisory quality, refresh, HEVC capability, and feature flags | Ignore or use the existing compatible setting; record a bounded stream adjustment. Required format, audio layout, transport, and encryption checks remain failures. |
| Moonlight launch metadata that is not used for media or input policy | Ignore malformed advisory values and record the field name and safe numeric value when available. HDR mode, audio selection, protocol version, and identity remain validated. |
| Initial or resumed Proton controller preference read | Use disabled or the last confirmed global value, respectively; client and application overrides still win. Bound the read and record the fallback. |
| Enabled Proton controller compatibility helper | If unavailable or unconfirmed, continue the application and record a warning. Retain an acquired helper reference until ordinary cleanup. |
| Configured application log | If unsupported, unopenable, or unattachable, continue with null output and report a warning. Invalid request syntax and unsafe nonregular output targets are still not used. |
| Best-effort detached command | Report executable lookup or spawn failures as warnings while the primary application continues. |
| Warning query and history submission | Run after the launch response and use bounded best-effort calls; absence, rejection, or uncertain delivery cannot invalidate the successful stream. |
| NVHTTP history-activity release (`ReleaseHistory`) | Failed, rejected, settled, or uncertain history releases are demoted to a warning and retried via an independently retained background queue keyed by exact `rtsp_operation` activity ID; pause, rejected resume, and cancel no longer destroy retained apps or delay later launches. |
| NVHTTP pause verification (`VerifyPaused`) and resume policy/display `settled` | When RTSP is confirmed retired, `VerifyPaused` rejection/settlement keeps the application paused; `RefreshResumePolicy` settlement falls back to the cached/disabled Proton policy; resume display `settled` rejects only the resume without killing the retained app. |
| NVHTTP initial Proton policy read (`ReadProtonPolicy`) | Rejection or settlement falls back to disabled Proton compatibility with a warning, matching `consume`. |
| Frame limiter planning (`frame-limiter`) | Unavailable policy, context, or non-recovery backend during read-only planning demotes to a no-op `Planned` response with a lifecycle warning. |
| Media transport audio config (`media-transport`) | Audio packet/channel/wire validation is skipped when `stream_audio` is `false`. |
| Optional lifecycle participants (`session-lifecycle`) | `ParticipantSettings::optional` prevents unapproved optional providers from stalling `approvals_complete` and skips read-only `ResourcePlan` rejections without failing the stream. |
| Optional extension and audio renegotiation probes (`stream-session`) | Rejected, timed-out or unconfirmed read-only probes leave the prepared session intact and report a warning. Unsettled broker operations remain bounded and retire independently; an unadmitted read-only probe is forgotten after the broker's 300-second admission recovery horizon. No format substitution is made. |
| Batch-scoped input refusals and unsupported optional feedback (`stream-session`) | The affected input or optional feedback is dropped while the stream continues. The session records bounded advisory issues for distinct refusal reasons. |

## Remaining boundary

The existing capture, display, and application retirement paths still require
case-by-case review before any further failure is demoted. In particular,
optional audio/input fallback (streaming video-only or view-only when hardware
devices are absent) remains gated by explicit user/session contracts unless
opt-in fallback policy is configured.

## RTSP request refusals

The negotiation parser reports whether a refusal left the pending request
unchanged. RTSP adjudicates `InvalidSequence`, `WrongState` and `Pending` as
request-only refusals. The prepared or playing ticket retains its resources
and deadlines. RTSP
records `negotiation_request_rejected` through the SDK lifecycle outbox with a
stable reason. A compatible retry may continue. A refusal response is never
reported as successful negotiation.

Epoch/session authentication, required wire/media validation, internal ticket
corruption and serialization failures remain critical. A lost response to a
noncritical refusal affects only that request connection; it cannot retire the
stream. An unacknowledged native start remains a critical ownership failure.
The PyroWave bitrate negotiation repair is a separate change.

## Common failure handler and terminal effects

`StreamFailureHandler` adjudicates bounded failure evidence at the owner of an
operation instance. Local reporters describe a failed requirement, a refused
request that left state unchanged, or an applied fallback. Authenticated
provider replies carry `StreamFailureFact` without a disposition. The decision
uses the original provider fact and the effect verified by the owner.
`StreamIssue` is the reporting and telemetry type and carries no authority.

RTSP binds wire completion to the exact pending action. A valid rejection
consumes that action and transfers one termination permit to its ticket.
Successful completion or request refusal invalidates the action. Reusing an
operation name creates a different owner instance. The same owner rule applies
to stream-session and NVHTTP record failures. Confirmed terminal observations
project an existing decision. Unconfirmed observations use a bounded deadline.

Providers can fence an unsafe local resource immediately. Video closes its
failed resource and reports the result. Stream-session fences media and input
when the binding is lost, then requests an authenticated coordinator pause if
the owner selects retirement. Coordinator invalidation alone only fences the
exact generation and plan. A separate owner pause authorizes release work.
Explicit stop, cancellation, shutdown and completed retirement use control
transitions. The native module ABI and provider failure JSON remain compatible.
The Rust source API for adjudication and RTSP rejection has changed.

The coordinator reads existing journal records. A binary predating this
change cannot reopen a journal containing an active fence-only record.
Complete the pending owner release before downgrading.

Pending RTSP request pressure uses bounded backpressure instead of an overflow
503. Noncritical refusal connection bookkeeping is bounded by the live transport
connection table. Existing deadlines, native retirement and ownership fences
remain intact. NVHTTP request/admission refusals before a ticket exists and
management errors are scoped to those operations and do not stop an existing
stream; local resource retirement in session-lifecycle is not a network decision.

Four active maintenance operations (`ForceIdr`, `FeedbackPeek`, `FeedbackAck`,
`PollGamepadFeedback`) use independent bounded continuation for timeout,
unavailable service, refusal, malformed optional reply and uncertain broker
bookkeeping. Their settlement queue is capped at eight, retains exact operation
identities and does not assert native retirement. Explicit recovery/cleanup
required, permissions, required input renewal, native media failure, selected
format/encryption, authorization and ownership persistence remain critical.

This enforces the migrated core boundaries, not native-code isolation. Trusted
native providers can still stop their own resources, fail or misclassify a
condition; the SDK cannot infer a safe fallback from arbitrary provider code.
The static inventory is a review aid, not a dominance proof, and skips symlinked
provider source. Provider/platform paths need separate source and runtime review
before claiming universal coverage.


History-free managed preparations keep their existing wire schema. A prepared
start refusal can be attributed by the selected provider's exact local session
identity (`ss-<hex>`) when the reply matches the already owned session, layer and
start phase, and confirms unchanged Prepared state with no cleanup error. The
owner binds that report to its stream before the common decision. Another
session, foreign scope, external capture or history-bound local alias is refused;
a critical report remains critical. Optional history is not an identity or
admission requirement.

### Prepared PLAY refusals

Stream-session returns a structured `stream_failure_fact` without a
disposition. RTSP verifies the provider's reply against the exact pending action
and the owned preparation, then uses the SDK's owner policy to decide whether
to refuse only the request or end that stream. An unverified native start
cannot receive a continuation permit. A late action completion leaves the
current action and its status alone. The old `stream_failure` reply remains for
compatibility with older RTSP consumers.

The coordinator also returns a preparation fact after it records a failed
attempt and begins locally owned rollback. NVHTTP checks the exact operation
and confirmed cleanup before adjudicating a required launch failure. A
retirement receipt lets RTSP project an already completed owner outcome.

The shipped workers use the SDK's standard policy. The RTSP controller accepts
an injected policy at authenticated request boundaries. The policy must preserve
required native and security fences.
