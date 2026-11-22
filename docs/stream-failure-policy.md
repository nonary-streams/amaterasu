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
| NVHTTP RTSP stop verification (`rtsp_cleanup_unconfirmed`) | Record an advisory warning while retaining the ticket for cleanup verification. A Quit response still reports incomplete cleanup when retirement cannot be confirmed. |
| NVHTTP pause verification (`VerifyPaused`) and resume policy/display `settled` | When RTSP is confirmed retired, `VerifyPaused` rejection/settlement keeps the application paused; `RefreshResumePolicy` settlement falls back to the cached/disabled Proton policy; resume display `settled` rejects only the resume without killing the retained app. |
| NVHTTP initial Proton policy read (`ReadProtonPolicy`) | Rejection or settlement falls back to disabled Proton compatibility with a warning, matching `consume`. |
| Frame limiter planning (`frame-limiter`) | Unavailable policy, context, or non-recovery backend during read-only planning demotes to a no-op `Planned` response with a lifecycle warning. |
| Media transport audio config (`media-transport`) | Audio packet/channel/wire validation is skipped when `stream_audio` is `false`. |
| Optional lifecycle participants (`session-lifecycle`) | `ParticipantSettings::optional` prevents unapproved optional providers from stalling `approvals_complete` and skips read-only `ResourcePlan` rejections without failing the stream. |
| Optional extension and audio renegotiation probes (`stream-session`) | Rejected, timed-out or unconfirmed read-only probes leave the prepared session intact and report a warning. Unsettled broker operations remain bounded and retire independently; an unadmitted read-only probe is forgotten after the broker's 300-second admission recovery horizon. No format substitution is made. |
| Batch-scoped input refusals and unsupported optional feedback (`stream-session`) | The affected input or optional feedback is dropped while the stream continues. The session records bounded advisory issues for distinct refusal reasons. |

## Launch admission review, 2026-11-22

Codex reviewed the changes to NVHTTP encoder selection and Moonlight query
parsing, and to command and environment validation from the catalog editor
through the host process launcher.

* For a fresh game launch, NVHTTP checks the available inventory before
  creating a record, then resolves the encoder after application preflight.
  Client, application, global and startup selections apply in that order.
  The owner refuses a known-empty inventory or an incompatible effective
  selection before launching the application or opening RTSP. A refusal after
  preflight records the specific cause on that attempt. Desktop, shared
  sessions and reconnects check the applicable settings before acquiring or
  replacing a transport. A rejected HDR reconnect leaves the live stream running.
* RTSP receives the effective codec offer and provider bounds. The client
  negotiates the format through ANNOUNCE before PLAY. Software encoding above
  the advisory threshold records a warning on the RTSP operation and keeps
  the requested mode. The selection checks consult a snapshot and issue no
  provider operations of their own.
* NVHTTP ignores the contents of Moonlight's unused optional `uuid`. Pairing
  and the bounded query parser still govern admission. The browser passes
  command suffixes and environment key spelling to the host. Host validation
  follows the operating system that will create the process.

Verification included a scoped selection opening RTSP, a scoped refusal
recorded before application launch, a rejected HDR reconnect preserving its
predecessor, and a 720p90 request opening RTSP with a performance warning.
The native x264 provider also produced a keyframe at that configuration.
A Linux process check launched an executable named `env.cmd` with two
environment keys differing only in case. These checks covered the source
checkout. Installed-package and client-playback qualification remain open.

## Catalog and application preflight review, 2026-11-22

Codex traced management catalog writes through catalog validation, launch
preflight, and native application dispatch. The catalog now accepts the
launcher’s executable and working-directory templates, bare commands, and up
to 64 literal arguments. The launcher still resolves the effective executable
to an absolute native path before dispatch. The editor and management route
preserve ordered detached helpers and their individual process settings.

During catalog preflight, a structurally valid optional log setting can expand
to an unusable path. Launch now discards that destination before opening a file
or reserving a preparation lease for it. It records the demotion on the exact
application operation, which NVHTTP associates with the stream. Preflight
retains the resolved choice for subsequent dispatch and replay. Application
launch, RTSP negotiation, and resource retirement then follow the existing
ownership and generation rules.

Source tests exercised a real child process starting with discarded output,
verified the published demotion and owner-visible warning, and checked
catalog path forms, the 64-argument boundary and per-helper settings.

## Remaining boundary

The existing capture, display, and application retirement paths still require
case-by-case review before any further failure is demoted. In particular,
optional audio/input fallback (streaming video-only or view-only when hardware
devices are absent) remains gated by explicit user/session contracts unless
opt-in fallback policy is configured.
