# Streaming latency diagnostics

Streaming providers and consumers emit bounded summaries through the normal SDK
host logger, approximately every ten seconds while they have work.
There are no per-input payload logs, key values, coordinates, controller states,
encryption keys or lease tokens. Logging failure never changes stream outcomes.
Optional network diagnostics use the public, versioned provider contract and SDK
adapter; existing v1 providers remain usable.

These summaries are debug records. A window is written every ten seconds
whether or not anything happened, and a full set of them is about a megabyte a
minute, which would push real events out of the rolled host log within
minutes. A host records them when its minimum log level is `debug`: set
`min_log_level` to `debug` in the bootstrap's `global` settings, or run a
prerelease build, whose default it is. The default log keeps events, including
the slow flows and spans of [flow tracing](flow-tracing.md). A module emits a
window with `diagnostics::log_window`, which applies this level.

These records are per-module aggregates. To follow one input event or frame
across modules, and to see which module held the host thread while it waited,
use [flow tracing](flow-tracing.md); the web UI's Diagnostics page presents it
as the Performance panel. `runtime.tick_holds` attributes host-thread time to
whichever module made the last broker call, so it can name the module ticked
just before the one that was slow. The `host.tick` spans of flow tracing time
each module call directly and should be preferred when the two disagree.

## Reading a capture

Run the same game, scene, stream format, resolution, frame rate, bitrate and client
settings for each comparison. Diagnostics continuously retain interval maxima,
small bounded samples of long observations and pending ages. Preserve the host
log, including rolled segments in chronological order. An already recorded
incident does not need to be reproduced to inspect its measured host stages.

```sh
python3 scripts/streaming_diagnostics.py /path/to/amaterasu.log
python3 scripts/streaming_diagnostics.py /path/to/amaterasu.log --json > timing-report.json
```

The reader accepts new structured records and existing capture, encoder and
transport records. It keeps the last 300 diagnostic records by default (bounded
to at most 1000 with `--windows`), groups stages by target and local identity,
and ranks their maxima. The reported p95 is the **worst window's upper bound**;
percentiles are never averaged or represented as a combined percentile. `--json`
includes the original windows, pending ages, backlog, unsupported observations
and deferral counters. Missing records can mean an older build, no relevant work,
log filtering or log loss. They do not establish zero latency.

## Measurement boundaries

| Record | What it measures | What to inspect |
| --- | --- | --- |
| `module.timing` | SDK module lifecycle tick/health duration and tick start-to-start interval, plus `direct_call_us` for [direct calls](direct-calls.md) the module served, for every module rebuilt with this SDK, including replacements | Names a module holding the cooperative host thread; a long interval with short calls instead indicates delayed scheduling |
| `service.worker` | Dedicated broker-service worker tick/health calls, reported separately from host-loop timing | Authentication, durable State, session history and launch coordination retain serialized service state while blocking CPU and storage work stays off the streaming host thread |
| `control.timing` | Socket receive calls and kernel timestamp age, protocol ordering/reassembly and event queues, reliable acknowledgements/retransmissions, decryption and authenticated queue residence | Pending assemblies and ages, receive budgets, socket memory/drop counters and unknown kernel timestamps; receive gaps are cadence, not proof of network lag |
| `stream.timing`, `input_batch_window` | Translated-event queue residence until batch scheduling; scheduled batch through provider acknowledgement, including journal and broker work | Queued/inflight events, high water, unknown associations, partial and cancelled batches, unacknowledged batch age |
| `runtime.invocation` | JSON decode/encode, shared broker mutex acquisition, broker/native dispatch and reply callback | Principal, dropped diagnostic observations, monotonic spike timestamps |
| `broker.timing` | Admission to provider acceptance, accepted handler to terminal result, first caller observation | Per caller/service identity, pending queued/inflight ages, cancellations/timeouts; one bounded record per identity |
| `audio.timing` | Backend capture reads and PCM worker queue residence | Pending PCM packet age, queue high water, recovery, discontinuities and drops |
| `host.timing` | Existing background host sampler plus Linux thread scheduler counters, kernel wait sites, process I/O/faults, pressure and owned UDP queues/drops | Cumulative counters require differencing matching TIDs; thread/socket caps and unavailable fields are explicit |
| `input.timing` | Enabled event injection/backend and repeat-policy call duration | Failed and suppressed events; excludes journal/broker wait and the game's consumption of OS input |
| `capture.presentation` | Existing VKMS source wait, export, snapshot and GPU-copy call counters | Changed frames, timeouts, sequence gaps and export-ahead observations; source wait can be intentional pacing |
| `video.pipeline` | Capture callback, copying, preprocessing, GPU processor dispatch, encoder submit/poll and transport admission calls; encoded-packet queue residence | Capture reuse, deferrals, drops, encoder rejection, presentation pacing and queue backlog; calls can overlap |
| `rtx-hdr`, `rtx_hdr_gpu_timing` | Existing provider GPU sampling/CUDA pipeline timings | Asynchronous GPU work differs from CPU dispatch time; read the selected provider's scope |
| `encoder.timing` | Provider input to owned output readiness; worker queue, submit/receive/drain calls; native GPU queue/conversion/encode timing when supplied | Provider latency tails, output-ready to later observation wait, pending/busy counters and native stage totals |
| `stream.timing`, `stream_forwarding_window` | Local bounded control, video and audio forwarding calls and pump intervals, including pending polls | Control budget exhaustion, pending PCM/processed PCM/audio and feedback; audio capture, optional processor submission/read, encoder submission/read and media admission also have separate call distributions |
| `media.processing_latency` | Existing independent host capture-start clock sampled at the start of media admission before packetization; packetization/FEC/encryption calls, socket flush, audio/video datagram queue residence to local network-provider acceptance | Unknown capture clocks, socket retries, queue high water and oldest pending age; local socket acceptance does not prove remote delivery |

Numeric stream IDs belong to their own providers. Use video records' encoder
session and the existing stream-session resource status to correlate providers;
equal numeric IDs across providers do not imply the same stream. Input context
indexes identify the current controller enumeration, not a persistent stream ID.
Optional/replacement providers retain their own behavior and timing availability.

`module.timing` emits every ten seconds while lifecycle calls occur, including
healthy windows. Tick CPU time and the wall time remaining after CPU accounting
separate execution from scheduling/blocking. That remainder includes preemption,
locks and IO; it does not identify a cause by itself. OS thread IDs align these
records with the background scheduler samples. Unsupported thread CPU clocks are
counted explicitly. `stages` identifies storage write, file sync, atomic replace
and directory sync calls on the owning module/worker thread.

`host.timing` uses the existing demand-driven host sampler, which runs for active
streams when host metrics are enabled; an absent record never establishes zero
pressure. Samples are built on the sampler worker, delivered over bounded queues,
and drained in small batches. GPU load and temperature are device observations,
not per-frame GPU encode timestamps. The Linux recorder script below can collect
OS evidence immediately without installing or restarting a host.

## Interpreting tails and unknowns

New `LatencyMetric` values expose `calls`, `total_us`, `min_us`, `max_us`,
`mean_us`, `p50_upper_us`, `p95_upper_us` and `p99_upper_us`. Each maximum
also has `max_observed_monotonic_us`; on Linux this uses the host-wide
`CLOCK_MONOTONIC`, shared with the OS observer. Up to eight most recent
observations of at least 8 ms retain their duration and monotonic timestamp,
plus an exact saturating count. These are observations, not automatic fault
labels: a cadence/presentation wait may be intentional. Samples use local
monotonic clocks, fixed storage and saturating counters. Empty metrics have
`calls=0`, null mean/percentiles/minimum and a compatibility `max_us=0`; an
observed zero duration instead has a positive call count. Histogram upper
bounds in microseconds are 50, 100, 250, 500, 1000, 2000, 4000, 8000, 12000,
16000, 24000, 32000, 64000, 128000 and 256000. The overflow bucket uses the
observed maximum. These are approximate conservative percentile bounds, capped
at the actual maximum, not exact sample percentiles.

Distributions reset at each report without discarding pending work's timestamp.
Input timestamps follow event coalescing, partial acknowledgement and cancellation;
missing associations are counted as unknown. Controller sample replacement keeps
the newest sample's timestamp. Input queue samples count scheduling attempts;
events rescheduled after partial delivery retain their original enqueue age.
Rejected control and video callbacks retain their
original queue timestamp and only count as completed queue samples when accepted.
Datagram queue distributions count datagrams, not frames. Video transport-admission
call timing records one aggregate observation per pump, rather than incorrectly
treating a batch's total as the time of each packet.

`encoder.timing` retains its existing cumulative worker counters and native-stage
totals; `latency_window`, provider input-to-output readiness and delivery-wait
distributions are interval values. Do not difference or sum the interval values
as if they were cumulative. GPU metrics remain empty when the selected backend
does not provide GPU timestamps. New SDK module diagnostics need rebuilt plugins;
existing native tables and wire/RTP presentation timestamps are unchanged.

Look for a large pending age even if completed samples are empty. A large
capture wait or frame interval may be expected at the configured cadence. A
large callback maximum with a low average identifies a spike; queue residence
or batch acknowledgement tails identify backlog or delayed dispatch. Use the
matching window and counters to decide which boundary needs a closer trace.

These intervals overlap and must not be added into an input-to-photon estimate.
Host instrumentation cannot measure the client's input sampling, one-way network
latency, game response/rendering, client decoder queue, presentation pacing or
display scanout. Those require client traces, clock correlation or an external
input-to-photon measurement. Source tests establish instrumentation behavior;
only a deployed build and an affected real stream establish the lag's cause.

## Dedicated service workers

Authentication password hashing/verification, durable State reads/writes, history
SQLite work and launch coordination use the public SDK's opt-in
`service_worker::ServiceWorker<M>` lifecycle adapter. The existing versioned
HostApi and module lifecycle contracts, caller identity, service selection,
request checks, reliable completion handling and persistence policies remain in
force. The adapter is for broker services; native instance callbacks and APIs
with host-thread affinity require their own explicit concurrency design.

Initialization and service registration finish before dependent modules start.
One worker owns the service for its entire lifetime. A single bounded wake slot
coalesces host ticks; it contains no request payloads or credentials. Host tick,
health and shutdown polling never acquire service state or wait for disk/hash
work. An in-flight callback finishes before its worker observes the shutdown
announcement; queued wakes then observe the newer phase before another call.
Providers continue their existing shutdown workflows while their dependencies
remain live. Quiescence succeeds only after affirmative provider readiness,
service destruction and a joined worker. Errors and panics remain visible to the
host without discarding the service before cleanup. Ordinary idle worker passes
do not notify the host, avoiding a wake feedback loop.

`service.worker` reports slow work on those dedicated workers. These durations
must not be interpreted as shared host scheduling holds. `module.timing` and
`runtime.tick_holds` continue measuring the host thread itself. Disk writes still
use their normal durability barriers, and password hashing retains its existing
Argon2 parameters. A source-validated candidate requires activation before these
changes affect a running stream.

## Immediate OS collection without a stream restart

```sh
python3 scripts/streaming_host_observer.py --pid HOST_PID --output /private/path/host-observer.jsonl
# Optional separate recording for the actual running game PID:
python3 scripts/streaming_host_observer.py --pid GAME_PID --output /private/path/game-observer.jsonl
```

The Linux recorder reads only the requested process's threads, counters and owned
socket rows, plus system pressure/network error counters. It verifies process
start identity before and after each sample, so PID reuse never silently joins
unrelated processes. It defaults to one sample/second for thirty minutes and a
four-segment 8 MiB evidence ring. No packet contents, game memory, command lines,
key values, controller samples or credentials are recorded. `sample_us` exposes
recorder cost. Counter resets and new threads have unknown deltas rather than
fabricated zero. Scheduler wait is an interval total, not a single stall length.

## Coverage and remaining physical boundaries

The report's `coverage` field names observed and missing host boundaries. Missing
records, unsupported provider capabilities and empty GPU timestamp distributions
remain visible; they are never converted into reassuring zero timings. Stream
input/forwarding windows include the owning resource IDs for cross-provider
correlation. Broker service rows and named storage stages retain their identities
across record ordering changes.

Host measurements establish how long work waited or ran inside the host. The
following require additional instrumentation at their actual owner: a game's
consumption of injected OS events, its simulation-to-render response, hardware
encode-engine timestamps on backends that cannot provide them, physical network
transmission/remote reception, and physical display scanout. A host receiving an
input or admitting a datagram cannot report those endpoints as measured. This
coverage accounting keeps those unknowns explicit while host investigation
continues; it does not assign the cause to the client.

Large diagnostic windows are split into independent JSON parts below the normal
host logger's message limit. Identity fields repeat, but each metric/counter is
emitted once. `diagnostic_part`, `diagnostic_parts` and the monotonic window ID
expose multipart records. Arrays can contain null holes in individual parts; the
reader keeps named stages and metric identities intact. Measurements are not
truncated merely because a stalled stream has many pending channels.
