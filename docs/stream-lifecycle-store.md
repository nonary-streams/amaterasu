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
is dropped and counted.

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
kept as unattributed and filed under the stream when the binding arrives; the
root cause is still the earliest critical failure by commit order.

All fields except `operation` and `site` must be short identifiers
(`[A-Za-z0-9_.:-]`, at most 64 bytes). An issue cannot carry a request body,
path or message. Invalid issues are refused by `report` and counted by the
subscriber.

A module needs the topic's `publish` grant in its component definition
(`crates/release/src/profiles.rs`, `STREAM_REPORT_TOPIC`). Without it the
runtime refuses the event; the module's own log line remains.

## Who reports what

The stream owner reports failures and fallbacks it observes from dependencies,
with `caused_by` identifying the dependency when known. A provider may also
report its own failure, but capture providers do not need lifecycle reporting
for the video owner to report a capture failure. NVHTTP keeps only its own stream warnings in the
store; warnings it relays for history were reported by their module.

## Dispositions

- `critical_failure`: the stream cannot start or continue. The stream's
  `root_cause` is the earliest critical failure that originated in its
  reporter (no `caused_by`), by commit order; a reaction stands in only until
  the origin's report arrives. Cleanup failures caused by it appear beside it.
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
complete; the reporters' log lines (`stream.lifecycle.*`) remain the complete
record. The store keeps the last 32 streams.

Not yet reporting: capture-vkms is a standalone workspace pinned to the
published `amaterasu-sdk` revision, which predates this module; its failures
reach the store only as the video service's reaction (`caused_by: capture`).
Encoder probes carry no operation ID, so a probe rejection appears only as the
video service's fallback.
