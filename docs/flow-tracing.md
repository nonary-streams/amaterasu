# Flow tracing

Flow tracing answers one question: when an input event or a video frame was
slow, which step took the time, and what was the host doing meanwhile.

The earlier [latency diagnostics](streaming-latency-diagnostics.md) report each
module's own stages as ten-second aggregates. They show that something was slow
somewhere. They cannot follow one event across module boundaries, and the
host-thread report blames whichever module made the last broker call. Flow
tracing follows the item itself and times each module call directly.

## The model

| Term | Meaning |
| --- | --- |
| Track | A family of flows, named once: `input`, `video`. |
| Flow | One traced item: one control message, one frame. A 64-bit identity both sides of a boundary derive from data they already exchange. |
| Stage | A point a flow reaches: `socket.arrived`, `session.dequeued`. |
| Step | The time between two consecutive stages of one flow. |
| Span | Work that occupied one thread: a module tick, a batch injection. An optional label says what it worked on. |

Each module marks only the stages it owns. The runtime joins the marks into
flows, keeps statistics for every step, and reports a flow that exceeds its
budget together with the spans that overlapped its slowest step. A report names
the step and the module that held the thread:

```json
{"event":"slow_flow","track":"input","total_us":23412,"budget_us":8000,
 "steps":[{"from":"socket.arrived","to":"control.authenticated","us":19810,"by":"amaterasu.control-transport"},
          {"from":"control.authenticated","to":"session.dequeued","us":3320,"by":"amaterasu.stream-session"},
          {"from":"session.dequeued","to":"session.submitted","us":12,"by":"amaterasu.stream-session"},
          {"from":"session.submitted","to":"input.accepted","us":270,"by":"amaterasu.stream-session"}],
 "slowest_step":{"from":"socket.arrived","to":"control.authenticated","us":19810,"percent":84,
   "during":[{"span":"host.tick","label":"amaterasu.state","us":21438,"overlap_us":19600}]}}
```

## What it costs

Recording one observation is one clock read and one push into a lock-free
queue: 62 ns on the producer in a release build
(`cargo test --release -p amaterasu-runtime recording_cost -- --ignored --nocapture`).
Nothing is formatted, allocated, locked or logged on the producer's thread.
Joining, statistics and reports run on the runtime's own `amaterasu-trace`
thread, which starts when the first module binds a sink and sleeps between
drains.

Every structure is bounded. A full queue drops observations and reports the
count in the next window; it never blocks a producer. A tracer without a sink is
one branch, so a module needs no fallback path for an older runtime.

## Recording from a module

```rust
use amaterasu_module_sdk::trace::{Stage, Tracer, Track, flow_id, names};

struct PacketTrace { tracer: Tracer, track: Track, read: Stage, admitted: Stage }

// Once, at start. Names are interned on this cold path.
let tracer = Tracer::bind(&runtime);
let trace = PacketTrace {
    track: tracer.track(names::VIDEO_TRACK),
    read: tracer.stage(names::SESSION_PACKET_READ),
    admitted: tracer.stage(names::TRANSPORT_ADMITTED),
    tracer,
};

// On the hot path. The video pipeline started this flow with the same identity.
let flow = flow_id(video_stream, packet.sequence);
trace.tracer.mark(trace.track, flow, trace.read);
// ...
trace.tracer.finish(trace.track, flow, trace.admitted, Duration::from_millis(12));
```

- A flow starts at its first mark and ends at `finish`. The module that ends a
  flow supplies the budget: the first-to-last time at which the flow is reported
  individually.
- `mark_at` records a stage that happened earlier, such as a datagram's kernel
  arrival: `tracer.now().earlier_by(kernel_residence)`. All timestamps use the
  sink's clock, so modules never compare clocks of their own.
- `mark_with` attaches one number worth seeing next to a slow step, such as a
  batch size. Never record user data, key values or coordinates.
- `span` returns a guard that records the work's duration when dropped;
  `record_span` records a duration measured some other way.
- A flow that never reaches `finish` is counted as abandoned after five
  seconds. End flows you start, including the paths that discard an item.

`Tracer` is `Copy`, `Send` and `Sync`. Keep the handles in the struct that does
the work; no global or macro is involved.

### Sharing a flow across modules

`flow_id(scope, sequence)` gives independently built modules the same identity
without a new field on any ABI. `scope` is a handle both already know, and
`sequence` the item's number within it:

| Track | Scope | Sequence |
| --- | --- | --- |
| `input` | the control stream handle | the authenticated message sequence |
| `video` | the video stream handle | the frame sequence carried by `EncodedPacket` |

Stage and track names in `trace::names` are the contract between modules. A
provider that replaces a built-in one joins the same flows by using the same
names and identity. A provider that records nothing leaves a gap in the steps,
not a broken trace: the step simply spans from the last stage before it to the
first one after.

### Current stages

| Track | Stage | Recorded by |
| --- | --- | --- |
| `input` | `socket.arrived`, `control.authenticated` | control transport |
| `input` | `session.dequeued`, `session.submitted`, `input.accepted` | stream session |
| `input` | `session.consumed` (not input), `session.coalesced` (replaced sample) | stream session |
| `video` | `video.capture_started`, `video.encoder_submitted`, `video.packet_queued` | video |
| `video` | `session.packet_read`, `transport.admitted` | stream session |

| Span | Label | Recorded by |
| --- | --- | --- |
| `host.pass` | — | host: one complete round of module ticks |
| `host.tick`, `host.health` | module identity | host: calls of at least 0.25 ms |
| `host.wait` | — | host: idle waits of at least 0.5 ms |
| `input.batch` | `direct` or `queued` | input |

The video pipeline starts each frame's flow and the consumer ends it. A consumer
that records nothing leaves those flows to be counted as abandoned, so a
replacement consumer of the video service should mark `transport.admitted` or
its own final stage.

`socket.arrived` is the oldest datagram a control stream read in the tick that
completed the message, which is the longest the message can have waited.
`input.accepted` is the input provider's acknowledgement. Host tracing ends
there: the game's consumption of the event, the network and the client are
outside it.

## Reports

All reports reach the host log under `amaterasu.runtime`:

| Target | When | Content |
| --- | --- | --- |
| `trace.slow_flow` | a flow reached its budget | every step, the slowest one, the spans during it |
| `trace.slow_span` | a span lasted 8 ms or more | the span, its label and its recorder |
| `trace.window` | every ten seconds with activity | per track: completed, slow and abandoned flows, total and per-step timing; the spans with the most time |

Slow reports are limited to twenty per track and twenty spans per window; the
window states how many were suppressed and the worst one. Percentiles are
histogram upper bounds, as in the other diagnostics.

Management reads the same history with `{"op":"trace_snapshot"}`: the last
minute of windows in full, a compact half-hour trend, and the recent slow flows
and spans, within one broker result. The web UI's Diagnostics page shows it as
the Performance panel through `/v1/diagnostics/performance`, which requires
`diagnostics.read`. Only management and Diagnostics may read the snapshot,
because it names every module.

## Contract

`TraceRecord` and `TraceSinkApi` are defined in `amaterasu-abi` and in
[`amaterasu.h`](../sdk/include/amaterasu.h). A module obtains its sink with the
`trace_bind` control message; the reply carries the process-local address of a
runtime-owned table that stays valid until the runtime is destroyed. The table
holds no module-supplied pointers, so binding needs no native grant: it carries
the same authority as writing a log record. The runtime attributes each record
to the endpoint that bound the sink; a record cannot name its own producer.

An older runtime rejects `trace_bind`, which leaves tracing disabled. A module
built before tracing never asks. The host binds a sink through its own endpoint
the same way.

`crates/module-sdk/src/trace/tests.rs` runs an unchanged consumer against a
second sink implementation that shares no code with the runtime's.
