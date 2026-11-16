# Stream lifecycle store

A stream launch crosses NVHTTP, RTSP, the stream session, the lifecycle
coordinator and its providers. When one of them refuses, fails or falls back,
the operator needs one answer: which module, doing what, and whether the
stream could continue. The stream lifecycle store keeps that answer. It is
shaped like a Vuex store.

| Vuex      | Here |
|-----------|------|
| mutation  | a `StreamIssue`: layer, phase, code, reason, disposition, fallback and the `file:line` that reported it |
| commit    | `StreamIssue::report` publishes it on the `amaterasu.stream.issue/1` topic; the runtime stamps the reporting module's authenticated identity |
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

Code that has no runtime at the point of failure, such as a record method,
pushes into a `StreamIssueOutbox` and flushes it from the module's tick with
`flush` or `flush_with`. The outbox holds 64 issues; when it is full the oldest
is dropped and counted.

`operation` is any operation ID the module holds for the stream: NVHTTP's
stream key (the `rtsp_operation_id` RTSP and the stream session receive) or an
ID NVHTTP derived from it (`<key>-…`, `<key>.…`, `<key>:…`). NVHTTP also
registers its application operation as an alias, so lifecycle coordinator
transitions (`<application operation>-lifecycle…`) reach the same stream.

All fields except `operation` and `site` must be short identifiers
(`[A-Za-z0-9_.:-]`, at most 64 bytes). An issue cannot carry a request body,
path or message. Invalid issues are refused by `report` and counted by the
subscriber.

A module needs the topic's `publish` grant in its component definition
(`crates/release/src/profiles.rs`, `STREAM_ISSUE_TOPIC`). Without it the
runtime refuses the event; the module's own log line remains.

## Dispositions

- `critical_failure`: the stream cannot start or continue. The first one a
  stream receives is its `root_cause` and is never replaced, so cleanup
  failures caused by it appear beside it, not over it.
- `demoted_warning`: a failure the stream survived. `fallback` names what it
  did instead and is required.
- `advisory_warning`: something worth knowing that needed no fallback, such
  as a stream warning a provider returned.

## What NVHTTP records itself

- Every launch failure path (`request_cleanup`, `fail_cleanup`,
  `reject_resume`, abandoned cleanup and transport) is a commit. The issue is
  credited to the layer of the job whose outcome was being handled, with the
  provider's own `code` as `reason`; outside a job it is credited to the
  record's stage.
- Every stream warning, including client advisory warnings and launch
  warnings, is an advisory issue.
- A refusal before any launch record exists (`private_error`, such as
  "another stream is active") becomes a stream of its own, `refused.<n>`, with
  the line that refused and the HTTP status.
- A history release that will not be retried is an advisory issue on its
  stream.

## Reading it

`POST /v1/nvhttp/launch_status` returns, for the selected launch,
`launch.lifecycle` with `root_cause`, `warnings`, `entries` (the last 64
commits), `committed`, `entries_dropped` and `audit_gap`. The reply also
carries `unattributed_issues` (reports that named no tracked stream, last 32)
and `lifecycle_feed_gaps`.

Each entry names the `publisher` (the runtime-authenticated module) and the
issue's `layer` (the part of the stream it concerns). They differ when a module
reports a failure it saw in a dependency: the stream session reports a video
encoder failure with layer `encode_video`.

## Limits

Events are notifications. If NVHTTP's subscription falls behind, the open
streams are marked `audit_gap` instead of presenting a partial record as
complete; the reporters' log lines (`stream.lifecycle.*`) remain the complete
record. The store keeps the last 32 streams.

Not yet reporting: RTSP, video, capture, display and audio report through
the stream session or NVHTTP rather than publishing themselves. Frame limiter
reports carry the coordinator's opaque provider operation ID, so they appear
under `unattributed_issues` until the coordinator passes the stream key to
providers.
