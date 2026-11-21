# Stream lifecycle store

A stream launch crosses NVHTTP, RTSP, the stream session, the lifecycle
coordinator and its providers. When one of them refuses, fails or falls back,
the operator needs one answer: which module, doing what, and whether the
stream could continue. The stream lifecycle store keeps that answer. It is
shaped like a Vuex store.

| Vuex      | Here |
|-----------|------|
| mutation  | a `StreamIssue`: layer, phase, code, reason, disposition, fallback and the `file:line` that reported it |
| commit    | `StreamIssue::report` publishes it on the `amaterasu.stream.report/1` topic; the runtime stamps the reporting module's authenticated identity |
| binding   | a `StreamBinding` from a consumer's operation prefix to the stream, so the providers it calls can report first-hand |
| state     | NVHTTP's `StreamLifecycleStore`: one `StreamLifecycle` per stream |
| getter    | `launch.lifecycle` in `POST /v1/nvhttp/launch_status` |

## Reporting from a module

```rust
use amaterasu_module_sdk::stream_lifecycle::{LifecycleLayer, StreamIssue};

// The stream cannot continue.
StreamIssue::critical(operation, LifecycleLayer::EncodeVideo, "wait_video_ready", "video_startup_failed")
    .because("encoder_unavailable")
    .report(&runtime)?;

// The stream continues without the feature.
StreamIssue::demoted(operation, LifecycleLayer::FrameLimiter, "resource_plan",
        "frame_limiter_unavailable", "noop_frame_limiter")
    .because("worker_not_ready")
    .report(&runtime)?;
```

The constructors record the caller's `file:line` (`#[track_caller]`), so a
helper that builds issues for its callers should be `#[track_caller]` too.
Critical and demoted constructors use their code as the reason unless the
reporter supplies a more specific reason. Older reports without a reason
remain accepted for compatibility.

Code that has no runtime at the point of failure, such as a record method,
pushes into a `StreamIssueOutbox` and flushes it from the module's tick with
`flush` or `flush_with`. The outbox holds 64 issues; when it is full the oldest
is dropped and counted. On the first publication attempt the SDK also writes
one human-readable line under the reporting component. A queued retry or
outbox transfer preserves that logging decision. NVHTTP logs its own local
commits and consumes provider reports without logging them a second time.

`operation` is the operation ID the module was called with for the stream.
Owners report their own reaction when a dependency fails; providers may add
their own reports when they support the lifecycle topic.

A module that calls providers for a stream under operation IDs of its own
publishes a binding first, so those providers' reports reach the stream:

```rust
outbox.push_binding(StreamBinding::new(operation_i_was_called_with, my_prefix));
```

The chain on a Linux launch:

| Caller | Binds | Providers then report under |
|--------|-------|-----------------------------|
| NVHTTP | its application operation (local alias) | RTSP and launch: the NVHTTP stream key or application operation |
| RTSP | `rtsp-<ticket>` to the NVHTTP stream key | stream session calls RTSP makes |
| stream session | `ss-<id>`, its `source-<sha256>` scope operation, its virtual display identity and the display scope ID to its `rtsp_operation_id` | video, audio, audio encode, transports, display, display settings, virtual display, input |
| video | `video-<epoch>-<id>` to the `ss-<id>-video` it was opened with | KMS, X11 and PipeWire capture and the encoder service: `video-…-capture-…`, `…-encoder-attempt-…` |
| lifecycle coordinator | each provider operation it allocates to the transition's operation | frame limiter, RTX HDR lifecycle provider |
| RTX HDR | `rtx-source-<scope>` to the display scope ID | display settings acquire for RTX HDR |

An operation matches a stream when it equals the key or an alias, or starts
with one followed by `-`, `.` or `:`; the longest match wins. A binding may
name its stream through an alias. Reports that arrive before their binding are
kept as unattributed and filed under the stream when the binding arrives.

All fields except `operation` and `site` must be short identifiers
(`[A-Za-z0-9_.:-]`, at most 64 bytes). An issue cannot carry a request body,
path or message. Invalid issues are refused by `report` and counted by the
subscriber.

A report can include `native_failure` when the reporter directly observed a
native call fail. It carries the versioned service, call, original signed
status code, and capture offer generation when known. Use the selected service
identity and the unmodified return code. This evidence names no paths, resource
handles, frame contents, or provider messages.
Older reports may omit it. Readers keep the base issue if optional native
details are unavailable or malformed, and ignore unfamiliar fields. Upgrade
the installed NVHTTP reader alongside a producer that emits this field if the
installed reader predates this compatibility behavior.

A module needs the topic's `publish` grant in its component definition
(`crates/release/src/profiles.rs`, `STREAM_REPORT_TOPIC`). Without it the
runtime refuses the event. Source logging is best-effort and independent of delivery.

## Who reports what

The stream owner reports failures and fallbacks it observes from dependencies,
with `caused_by` identifying the dependency when known. A provider may also
report its own failure, but capture providers do not need lifecycle reporting
for the video owner to report a capture failure. NVHTTP keeps only its own stream warnings in the
store; warnings it relays for history were reported by their module.

## Dispositions

- `critical_failure`: the stream cannot start or continue. The stream's
  `root_cause` is the earliest critical failure reported first-hand or observed
  directly at a native call, by commit order. A downstream reaction stands in
  until that evidence arrives. Cleanup failures caused by it appear beside it.
  A critical failure stops being the root cause when it is recovered from:
  its reporter later demotes the same layer and code for the same operation, or a
  caller demotes with `caused_by` its layer and its code as `reason` (a
  fallback to another encoder or capture provider).
  A fallback with phase `provider_fallback_confirmed` names the failed
  provider attempt as its operation. Emit it only after that attempt is retired
  or definitively held no resources. Only that attempt is recovered, even if
  its report arrives late or names a more specific provider error; failures
  from other attempts remain critical.
  A capability skipped before any provider attempt starts is a local fallback
  and must not mark the operation of the subsequently opened provider retired.
  Recovery of a secondary failure is retained while that failure remains in
  the bounded history, including when an earlier report is attached after its
  stream binding arrives.
- `demoted_warning`: a failure the stream survived. `fallback` names what it
  did instead and is required.
- `advisory_warning`: something worth knowing that needed no fallback, such
  as a stream warning a provider returned.

## What NVHTTP records itself

- Every launch failure path (`request_cleanup`, `fail_cleanup`,
  `reject_resume`, abandoned cleanup and transport) is a commit under the
  `nvhttp` layer, `caused_by` the layer of the job whose outcome was being
  handled (or the stage it waited in), with the provider's own `code` as
  `reason`.
- Its own stream warnings, including client advisory warnings, are advisory
  issues.
- A refusal before any launch record exists (`private_error`, such as
  "another stream is active") becomes a stream of its own, `refused.<n>`, with
  the line that refused and the HTTP status.
- A history release that will not be retried is an advisory issue on its
  stream.
- Stream-session prepare and start requests that are refused without stopping
  an existing session report an advisory issue when the caller is authorized
  to name that stream. A caller that cannot continue reports its own critical
  reaction. A lost initial arm completion that requires rollback is critical.
- The stream session reports an audio-encoder reset as a demoted warning only
  after the reset succeeds. Unconfirmed cleanup is critical; a later confirmed
  cleanup demotes that cleanup failure without erasing an earlier launch failure.

## Reading it

`POST /v1/nvhttp/launch_status` returns, for the selected launch,
`launch.lifecycle` with `root_cause`, `warnings`, `entries` (the last 64
commits), `committed`, `entries_dropped` and `audit_gap`. The reply also
carries `unattributed_issues` (reports that named no tracked stream, last 32)
and `lifecycle_feed_gaps`. `recent_refusals` contains up to 16 retained
pre-launch refusal snapshots, newest first, even when no launch was created.
Refusals do not evict an open stream: if all store slots are occupied by open
streams, the refusal appears among the bounded `unattributed_issues` instead.

Each entry names the `publisher` (the runtime-authenticated module) and the
issue's `layer` (the part of the stream it concerns). They differ when a module
reports a failure it saw in a dependency: the stream session reports a video
encoder failure with layer `encode_video`.

## Trust

Any module granted the topic can publish a binding, so a module could file
another stream's reports under the wrong launch. Publishers are installed,
signed components with an explicit grant, the same trust the rest of the
control plane relies on. The publisher identity on every entry is the
runtime's, not the payload's.

## Limits

Events are notifications. If NVHTTP's subscription falls behind, the open
streams are marked `audit_gap` instead of presenting a partial record as
complete. The console requests `{"issues_only":true}` from `launch_status` with `session.read`.
The getter returns up to 32 recent commits across retained streams, including
ended attempts, refusals, and reports awaiting attribution.
It groups repeated commits for the same operation and shows an incomplete
history when the subscription is unavailable, entries were dropped or the
feed skipped records. Source logs can help investigate missing events. The
store keeps the last 32 streams.

The in-tree VKMS capture build uses the host SDK and publishes its own issues.
Encoder probes carry no operation ID, so a probe rejection appears only as the
video service's fallback.

## Logging change review, 2026-11-18

Codex reviewed the SDK reporting and retry paths, NVHTTP launch monitoring,
RTSP ANNOUNCE and PLAY, video provider fallbacks, retained session warnings,
and the console read path against the stream failure, admission, retirement
and generation isolation contracts.

- NVHTTP commits its own decisions under the launch operation. A provider
  publishes its issue through the SDK. A failed log write permits publication,
  and a failed publication leaves the report queued. Moving a ticket's pending
  reports to the controller preserves the logging attempt. The SDK regression
  `publication_retry_after_retirement_keeps_the_issue_and_logs_once` exercises
  both failures.
- After a successful ANNOUNCE reply, RTSP reports the accepted adjustments
  under that ticket's operation. The standard and extension paths pass the
  supported client feature bits to the stream. Their status and history use
  the same adjustment values. RTSP writes the playback summary after the
  stream owner accepts the start and the PLAY reply is sent. The RTSP tests
  exercise unknown optional flags, distinct tickets and effective bitrates,
  extension negotiation, and rejection of a mismatched required profile.
- Video tests exercise a running replacement encoder and a recovered capture
  before reporting fallback. NVHTTP's monitor test verifies one source log
  line from the committed failure and silence during healthy polling. Quit
  tests cover a warning for unconfirmed cleanup and preservation of application
  cleanup authority. Late lifecycle preparation tests cover a closed ticket.
- Diagnostics reads the store through the session-read route. The browser
  tests exercise different operations, dismissal, missing data and access
  without host-log permission. Logger tests drive sixty ten-second windows at
  both Info and Debug and check rotation. These are source and fixture results.
  Installed-package activation and physical client playback require a separate
  run.

## Capture failure review, 2026-11-19

Codex reviewed the VKMS presentation worker, Video's `latest_presentation`
call and stream-session's native HDR and video receive paths. The review
followed authenticated RTSP PLAY through the owned video source and checked the
existing stream-session retirement and NVHTTP lifecycle decision for a failed
source.

- VKMS records the first data-path fault under its capture operation and emits
  `capture.failure` with the capture ID, CRTC, last delivered sequence and
  reason. Helper wait and export failures retain the helper result and available
  errno. GPU snapshot failures identify the failed step and retain its native
  return code when applicable. The worker keeps the existing retry budget and
  retains GPU source guards until retirement is proven.
- Video preserves the provider and return status from `latest_presentation`
  in the capture dependency report. Stream-session leaves a closed HDR query
  to the video receive path, which reports `video_stream_closed`. Invalid HDR
  metadata still fails through stream-session, and valid metadata uses the
  existing negotiated HDR signaling. These observations keep the existing
  operation ownership and reach the launch owner without changing ticket
  admission, encryption, media format or cleanup authority.
- The capture logger and lifecycle reports remain outside stream decisions.
  Regression tests cover retry followed by a terminal export error, once-only
  logging with the original errno, Video's capture status publication, HDR
  signaling and close ordering. The capture, Video, stream-session and RTSP
  suites passed. These are fixture results. The triggering fault in the original
  disconnect remains unknown.
