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
corruption and serialization failures remain critical. Failure to deliver a
refusal still closes the ticket as `response_unavailable`; disconnect and
ownership retirement fences still apply. Backend acceptance remains a separate
obligation: recoverable media hints must be negotiated before completing an
ANNOUNCE, rather than passing `false` to `Negotiation::complete`, whose failure
is terminal. The PyroWave bitrate negotiation repair is a separate change.
