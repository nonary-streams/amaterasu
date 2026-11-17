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

`amaterasu_gamestream::rtsp::negotiation::Error::is_critical` owns the
classification of negotiation errors. `InvalidSequence`, `WrongState` and
`Pending` reject only the current request: negotiation has made no state change,
and the prepared or playing ticket retains its resources and deadlines. RTSP
records `negotiation_request_rejected` through the SDK lifecycle outbox with a
stable reason. A compatible retry may continue. A refusal response is never
reported as successful negotiation.

Epoch/session authentication, required wire/media validation, internal ticket
corruption and serialization failures remain critical. A lost response to a
noncritical refusal affects only that request connection; it cannot retire the
stream. An unacknowledged native start remains a critical ownership failure.
The PyroWave bitrate negotiation repair is a separate change.

## Common failure handler and terminal effects

`StreamFailureHandler::decide` is the synchronous SDK decision point. Lower
layers submit typed `StreamFailure` reports, with explicit disposition, reason
and fallback. Only a valid critical report creates the private
`StreamTermination` capability. Warning, demotion and invalid reports cannot
execute terminal effects. Publishing diagnostics to the lifecycle store grants
no authority.

The RTSP action completion boundary preserves classification across provider
RPC replies, checks exact operation scope, and invokes the handler before
changing negotiation or rejecting the stream. Recoverable prepared-session
refusals clear only the pending transaction and allow a corrected request.
Required failures from older providers without a valid typed report remain
critical. The public boolean completion API is removed: `complete(epoch,
ticket)` means success, and `reject(epoch, ticket, &StreamTermination)` requires
a critical decision. This is an intentional Rust source API change; the native
module ABI is unchanged.

The stream-session job, native media/input/journal and cleanup failure effects,
RTSP record closure/cleanup and transport protocol/security failures, and
NVHTTP record failure/cleanup and failed-launch response effects use the same
handler. Bare error strings no longer silently become terminal decisions in
these adapters. Normal authenticated stop, cancellation, quiescence and physical
peer/transaction completion retain explicit control paths.

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
