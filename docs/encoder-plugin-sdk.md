# Encoder provider SDK

An encoder can be built and shipped as an independent native component with its
own versioned service. Native modules are trusted code loaded into the worker;
service grants govern routing and do not sandbox native code. The interfaces
below are draft. The C declarations are in `sdk/include/amaterasu_media.h`,
`sdk/include/amaterasu_encoder.h`, and `sdk/include/amaterasu_encoder_input_pool.h`;
Rust declarations are exported through `amaterasu_module_sdk::media_abi`.
Check the interface major and table size before calling an optional table.

## Integration boundaries

The existing video pipeline uses the legacy encoder control service and native
encoder tables. Its CPU path submits a borrowed CPU frame synchronously; success
means input was accepted or copied, while busy accepts nothing. The Linux
DMA-BUF path requires compatible advertised input and encoder interfaces. Its
leased variant pins immutable pixels and requires acquire and completion
synchronization. Bindings must outlive the leases and their completion fences.
See [Capture provider SDK](capture-plugin-sdk.md) for the frame contracts and
`modules/encode-video/encode/README.md` for built-in backend capabilities and qualification.
Those capabilities do not automatically apply to a third-party component.

`EncoderProviderApi`, `FrameResourceConsumerApi`, and `EncoderInputPoolApiV1`
are independent optional native interfaces. Their definitions do not by
themselves connect a provider to video. The current video source contains
source-driven and generic resource-consumer routes described below. A signed
single-host test exercises both with synthetic fixtures, including clock and
callback negotiation and cleanup. The writable pool is not connected to
video. A provider and consumer must explicitly negotiate supported formats,
resource schemas and controls. A GPU resource offer
is not proof of a no-copy path, hardware support, or client playback.

## Register a legacy encoder service

A component using the existing video path registers a control service and its
native encoder table under the same versioned service name. Its control service
must enumerate, probe, open, lookup and close sessions with authenticated
caller ownership and bounded cleanup. The signed component set must include
the component and a startup dependency, plus video call and native-use grants
for that service. Registration does not install the component or grant access.

Register a bounded alias in video startup settings and choose that alias in the
launch request:

```json
{
  "enabled": true,
  "external_encoder_providers": {
    "vendor": "example.encode/1"
  }
}
```

Set `encoder_provider` to `"vendor"` at launch. Omitting it selects the built-in
`amaterasu.encode/1`; an explicit selection does not automatically fall back to
another provider. The service comes from trusted startup configuration, rather
than a service name supplied by the launch request. Configure at most 16 unique
version-1 service names, distinct from the built-in service. Aliases are at most
64 characters and use lowercase letters, digits, `_` or `-`. Every configured
service needs the startup dependency and call/native-use grants, even if no
launch has selected it. The current route uses the legacy control protocol and
`EncoderApi` 1.0, with optional 1.1 and 1.2 DMA-BUF tails. An explicitly
selected external provider can also receive the bounded `encoder_options`
object described below; the provider validates it. The independently built `fixtures/encoder-source` component illustrates
the legacy CPU interface and caller-scoped control. It advertises fake H.264,
uses a two-packet queue and retries rejected output and EOS; its test payload is
not decodable video and does not demonstrate playback, real source capture or
DMA-BUF input. Its separate `example.encode-provider/1` service also demonstrates
a source session with the same caller-scoped control session ID. It produces one
fake access unit without frame submission, supports notifications, and retains
bounded idempotent control results. The fixture README describes its exact
configuration and supported controls. Its standard limits probe advertises
128 control records and 128 KiB of bounded replay storage. Its generic CPU
consumer validates the negotiated clock and sequence policy without retaining
CPU frames.

## Source and generic resource routes

The current video implementation registers a source provider through signed
startup settings with distinct control and native services and allowlisted,
nonzero source IDs:

```json
{
  "source_encoder_providers": {
    "vendor": {
      "control_service": "example.encode/1",
      "native_service": "example.encode-provider/1",
      "source_ids": [42]
    }
  }
}
```

Grant the control call and native use and include both startup dependencies.
Open with that `encoder_provider` alias, an allowed `source_id`, and an explicit
encoder identity; a source open does not require capture output. Optional
`encoder_options` is an object of at most 64 KiB forwarded to control open and
included in operation idempotency; the provider validates or rejects it. Video
uses the standard common configuration for 8-bit SDR 4:2:0 and supports H.264
and HEVC Annex B or AV1 OBU framing on this route. Provider probe and negotiation
still decide actual capability. The fixture validates H.264 routing with fake payloads; HEVC and AV1 native
providers, real capture, encoding and playback require independent
qualification.

The current generic submitted-input route pairs an existing external legacy
encoder alias with a distinct native resource-consumer service through
`external_encoder_consumers: {"vendor":"example.consumer/1"}`. The same alias
selects it at open. Video negotiates capture v1.8 and consumer resource offers
and requires a clock and sequence handshake. Capture open receives
`resource_capture_clock` with `domain: "host_monotonic"` and a 32-character
lowercase hexadecimal epoch ID; it must echo that value and supply frames with
matching host-monotonic epoch and nonzero strictly increasing sequences.
Video commits a sequence only after accepted submission. Encoder open receives
`resource_output_clock` with `domain: "stream_relative"`, the same epoch ID
and `host_origin_us`; it must echo that object and return
`resource_sequence_policy: "strict_source_echo"`. The route does not silently
remap resource descriptors. Cached repeated or nonincreasing inputs fail closed. Capture and encoder open
also each receive `resource_callback_contract: "cross_service_safe"` and must
echo it exactly; an absent or different acknowledgment rejects the path before
nested calls. Capture offer enumeration can synchronously call consumer
configure, and a capture frame callback can synchronously call consumer submit,
on the same owning thread. Providers must permit this cross-service nesting,
avoid a shared non-reentrant lock or other deadlock, respect borrowed callback
lifetimes and honor cancellation barriers. The generic route retains capture
and consumer bindings through graceful drain and EOS. This does not connect an encoder-owned writable pool or
establish GPU compatibility. The signed fixture test covers this route with CPU resources and fake output;
deployment and hardware qualification remain separate.

## Sessions and input negotiation

The authorized control service creates, looks up and closes principal-scoped
sessions with idempotent operation IDs. The separately registered
`EncoderProviderApi` probes a candidate without side effects and configures an
already authorized session for submitted-input or source mode. Provider,
resource consumer and input pool use distinct signed services and grants. The
host selects a coherent set belonging to the same provider and authenticated
owner; they share the session ID namespace and lifetime and reject foreign or
closed sessions. For submitted mode, that session ID is the stream ID on the
associated `FrameResourceConsumerApi`. A source session produces its own output
and receives no frame submissions. Codec, framing and options use
negotiated namespace/value/schema identifiers; unknown IDs are not inherently
invalid, but must be understood by both participants. Options are borrowed for
the probe or configure call and limited to 64 KiB. Probe returns bounded
supported limits and effective format through a negotiated response schema;
unknown IDs still require negotiation. The standard `EncoderProviderLimitsV1`
probe response reports per-session control and output bounds.

For submitted input, enumerate consumer offers and the producer's offers. Check
the complete resource and synchronization schemas, format, dimensions and device
identity; matching kinds alone do not establish compatibility, and a zero device
identity means unknown. Configure the consumer with the selected producer offer
first, then select the producer. If either operation fails, do not submit;
re-enumerate and configure again. Offer data is borrowed only during the call,
so copy anything needed afterward. An incompatible or stale consumer
configuration returns `MEDIA_UNSUPPORTED` without changing its previous state.

A resource submission borrows the frame synchronously. Before returning, the
consumer must complete its accesses or retain a reference for each asynchronous
use. A failed submission retains no work; successful asynchronous work must
release each reference after completion or with the negotiated completion
fence. `FRAME_RESOURCE_SUBMIT_FORCE_IDR` must force codec recovery or return
`MEDIA_UNSUPPORTED` without consumption; unknown flag bits are likewise rejected
without consumption. Wait for the acquire synchronization before accessing the
resource.
Resource descriptors and retain/release rules are specified in
[Capture provider SDK](capture-plugin-sdk.md). Retain and release are module-thread
operations unless the schema explicitly promises more. Cancel stops admission
and synchronously retires retained references and pending work; keep the owning
binding alive through cancellation.

The standard `EncoderCommonConfigV1` options schema describes coded and visible
geometry, rational FPS, bitrate, profile, level, chroma, bit depth and optional
color. The visible rectangle must fit the coded size; FPS is nonzero. A zero
bitrate, chroma or bit depth means unknown, and profile, level and color use
presence flags. Unknown required values can be unsupported. Reserved fields
and unknown flags must be zero. This is a native ABI struct, not a network wire
format; check its version and size. Its output clock is v1 stream-relative with a nonzero epoch;
`output_clock_origin_us` maps that epoch to host-monotonic time using checked
addition. Reject unsupported or unestablished clocks instead of inferring an
origin.

The Rust SDK exposes `encode::FrameCadence` and `encode::CommonConfigV1` for
authoring and reading this schema. `FrameCadence::from_fps(60)` preserves the
legacy `60/1` representation; `FrameCadence::from_millihz(59_940)` represents
exactly `59_940/1000` FPS. Set the cadence on a validated common config, then
call `SessionConfig::set_common_config_v1`; providers can read a negotiated
standard config with `SessionConfig::common_config_v1`. Other option schemas
remain opaque, and milliHz conversion is reported only when exact.

Optional `EncoderFrameOptionsV1` carries frame identity, source epoch, clock
and independently negotiated side data. Prepare it through frame-options control
and wait for `APPLIED` before submission. The nonzero submission ID is unique
within the session and must match the resource frame sequence, or both pool
submission sequence and control ID. A successful submit consumes preparation
once; a rejected submit consumes nothing, so retry the same frame and metadata
or cancel. Conflicting, stale, foreign or reused IDs are rejected. Cancel,
close, and source or configuration generation change discard unconsumed
preparations. Retained side data must be copied, and prepared count and bytes
are bounded; excess returns `MEDIA_BUSY`. Pending preparation does not authorize
submission.

## Output, backpressure, and epochs

`receive` delivers at most one chunk to a synchronous sink. Its `max_bytes` must
be between 1 and 16 MiB; the provider splits larger output as needed. The chunk
and its bytes are borrowed only until the sink returns. `STATUS_OK` consumes it;
a rejected sink returns its status and leaves the identical chunk available for
retry. If the next bound is smaller than a rejected chunk, `MEDIA_BUSY` is
returned without invoking the sink; retry with the original bound.
`MEDIA_PENDING` invokes no sink. Providers must bound their output storage and
propagate backpressure instead of accumulating an unbounded backlog.

Chunks belonging to an access unit are ordered and share identity, timing and
format fields. START and END flags mark the unit boundaries; picture flags apply
to the whole unit. `submission_id` is the input sequence for submitted mode, or
zero when no input corresponds to the output. `source_id` and
`config_generation` identify source and configuration epochs. Timestamps are in
microseconds in the negotiated session clock. Keep old queued chunks tagged with
their original generation after a configuration change. EOS is an empty chunk
with no picture boundary flags.

The Rust SDK's `encode::OutputQueue` provides bounded owned output, a shared
front and explicit acknowledgment after acceptance. Its source and submitted
modes, drain/finish, cancel and generation-based wait are local building blocks.
`receive_native`, `subscribe_native` and `unsubscribe_native` provide sink retry
and a synchronous notification lifetime barrier. The SDK also exports
`EncoderProvider`, `ProviderAdapter`, `SessionConfig`, `ControlRequest` and
`ControlResponse`. The provider author still owns authentication, the shared
owner and session namespace, control idempotency and retained results, resource
retirement and worker shutdown. `ControlPayload::FrameOptions` carries
`OwnedFrameOptions`, including a deep copy of nested side data; other payloads
use `ControlPayload::Opaque`. Owned frame options preserve bounded v1 trailing
extension bytes for request equality; v1 extensions must be self-contained
and pointer-free. Pointer-bearing extensions need a new negotiated schema. The example at
`crates/module-sdk/examples/encoder_provider.rs` demonstrates a source queue:

```sh
cargo run --locked --offline -p amaterasu-module-sdk --example encoder_provider
```

## Controls, notifications, and shutdown

Control requests have caller-chosen nonzero IDs unique within the session, an
expected generation and negotiated schema/data. Payloads are borrowed for the
call and limited to 64 KiB. The operation kinds are reconfigure, request
keyframe, recovery, effective state, frame options and poll; their presence in the ABI does not
establish provider support. Providers and callers must agree on payload schemas
and verify actual results. IDs are deduplicated per session: retrying the same
ID and payload retrieves its stored result; a different payload with that ID is
rejected. Poll uses the original ID with empty data. Request and result storage
survives until session close. Retained requests, results and tombstones are
limited to 1,024 records and 8 MiB per session, or lower advertised limits. When
full, new IDs return `MEDIA_BUSY` before side effects; accepted retries and poll
remain admissible.

Responses echo the ID and include an outcome and effective generation and
submission boundary. `ACCEPTED` and `PENDING` do not establish effective state;
`APPLIED` and `CLAMPED` report actual state. `UNSUPPORTED`, `RECREATE`, `FAILED`
and `UNKNOWN` are distinct outcomes. A response does not mean a requested
keyframe or recovery has already appeared in output. A rejected sink retains
the exact response for retry and does not undo any hardware effects.

Optional event subscription functions are present together, with at most one
subscription per session. Callbacks may run concurrently on provider threads,
coalesce notifications and must only record the event and wake the consumer;
they must not reenter the provider or unwind across the ABI. Subscription
announces already available output or a stopped state. Unsubscribe is an
idempotent synchronous barrier for admitted callbacks and must not be called
from a callback. Per-session operations otherwise are serialized by the caller.
In the Rust provider adapter, `EncoderProvider::notification_queue` returns
`Ok(Some(queue))` for a queue whose unsubscribe provides this barrier, or
`Ok(None)` to certify no callback or subscription exists, including for an
unknown or closed session. An error or panic aborts the process because the
void ABI cannot report an unproven barrier.

Drain stops new input, completes accepted work, and emits exactly one EOS after
that work; repeated drain is idempotent. Receive through accepted EOS if all
output is required; cancellation may discard accepted work. Before provider destroy, control close or provider release,
the pool must report `RETIRED`. Destroy synchronously unsubscribes, cancels resource
submissions and retires work; the control service then closes the session.
Releasing the provider performs the same barriers for its sessions. Keep all
relevant bindings alive through cancellation and destruction. In the Rust adapter,
void unsubscribe, destroy and release abort the process on panic or inability to
prove their barriers; a release error is fatal. Leaking an allocation cannot
pin host-loaded module code. Neither operation promises asynchronous retirement; never let a callback or borrowed pointer outlive its documented call.

## Optional encoder-owned writable pool

`EncoderInputPoolApiV1` describes an independent optional pool. The SDK
contains pool lifetime helpers and a CPU-copy writer, but video does not wire the
pool into its production path. The host cannot yet guarantee that a quarantined
pool pins the required module generation and worker runtime through unload; a native binding
alone is insufficient. The CPU helper is not GPU or zero-copy qualification.
Enumeration returns resource offers and a nonzero offer generation, including
for an empty list. Configure validates the full selected offer, device, size,
format and sync compatibility. It is atomic on failure; success produces a
fresh nonzero pool generation. The pool is bounded to 1–64 targets.

Acquire returns an exclusive target lease, or `MEDIA_BUSY` without creating one.
The target's generation, target ID and lease ID are nonzero; lease IDs are not
reused within a stream. Resource descriptors and handles remain borrowed until
the lease is consumed. The writer waits on acquire synchronization and any
resource-specific plane fences before access. `NONE` asserts previous encoder
reads completed. After writing, commit supplies completion synchronization
covering all writer work; `NONE` asserts that work is complete. The encoder
must duplicate needed handles before returning success and wait before reading.
A successful commit consumes the lease and queues encoding; failure consumes
nothing and schedules no read. Abandon similarly consumes a lease only on
success, with completion covering all writer accesses. The submission sequence
identifies output input; a nonzero control ID identifies the agreed frame-control
transaction, not an address. When using prepared frame options, both values
equal the prepared submission ID and the session, source epoch, clock and
timestamp match.

Reconfigure stops new acquisitions from the old generation. Old leases may be
abandoned, but stale commits fail and remain owned by the writer. Providers
reject stale, duplicate, wrong-stream and consumed identities; they must not
reuse storage until all writer and encoder accesses are safely complete. Failed
or invalid fences require quarantine. Stop writers and resolve all leases
before cancel, without concurrent pool calls. `begin_retirement` returns a status;
on success it stops acquire, configure and commit, and outstanding leases must
still be abandoned with completion. Retirement is `MEDIA_PENDING` while work
remains, then `RETIRED` or `QUARANTINED`. Until `RETIRED`, retain pool and
encoder bindings, code, source, device and storage until independently proven
safe or process exit; an unknown result or timeout is not proof of completion.
Process exit ends in-process ownership but does not prove external GPU, DMA or
shared-handle access is safe. Termination requires platform or driver cleanup
guarantees or independent completion evidence.
A native binding alone does not pin module code or the runtime through unload.
A quarantine guard must own the actual code, runtime, device and source
lifetimes, or use process-scoped supervisor support that guarantees them. Leaking
a binding or allocation alone is insufficient.
Cancel synchronously retires
accepted work and fences or quarantines unsafe storage while retaining required
resources and bindings. `RETIRED` means completion is known; `QUARANTINED`
means it is not. A device
reset alone does not prove completion.


The public Rust `encode::cpu_pool_writer` helper negotiates a compatible
host-addressable pool target and synchronously copies borrowed BGRA8 or NV12 CPU
capture into it. The caller independently selects a matching capture format,
prepares any frame control, and owns pool retirement and a real lifetime guard.
Busy acquisition creates no lease. If commit fails, the helper attempts abandon;
if abandon fails it returns the still-live lease for explicit retry. Its unsafe
boundary requires valid immutable capture storage, an ABI-compliant provider and
exclusive valid writable targets. `encode::pool::InputPool` and `InputLease` are
not `Send` or `Sync`; dropping an unresolved lease poisons the pool and leaks its
guard, which protects dependencies only if its construction contract is met.
The existing host native binding alone does not meet that contract. Fake tests
exercise the helper; they are not production or hardware proof.

## Optional measured timing

`EncodeTimingSnapshotV1` and the C `AmaterasuEncodeTimingSnapshotV1` define
cumulative input-to-owned-coded-output-ready observations for one session epoch.
Optional additive tails are encoder v1.3, encoder-provider v1.1, and video v1.4;
all older prefixes and mandatory minimum versions are unchanged. The native
family identities and signed service grants remain required. A query authorizes
the same session owner as receive, initializes output only on success, consumes
no media, and performs no work-retirement or worker-result side effects.

Providers timestamp readiness when complete owned coded output is produced,
before it enters a consumer queue. Pair that with the corresponding admitted
input's monotonic start. This measures provider conversion, queueing and encoding
through the output-readiness observation. An asynchronous codec may complete
internally before that observation; the contract does not claim device-only
execution time. Exclude later consumer polling and sink retries. Unmatched,
failed, unsupported or unmeasured work adds no sample. Never use admission as
completion or subtract timestamps from unrelated clocks.

Counters are exact, bounded, and scoped by a nonzero epoch; change the epoch
before resetting them. Stop adding samples before integer overflow. A zero
sample count is unknown, whereas a measured zero duration is valid. Each duration
is at most 60 seconds; a stalled frame outside that bound remains unmeasured.
Consumers must use compatible interval deltas, reject backwards counters and
avoid joining epochs. Video translates epochs when the selected provider or
session changes. Native API reads never silently choose another encoder.

Rust providers may implement `EncoderProvider::encode_timing`; the SDK adapter
publishes the new tail and defaults to unavailable. Rust consumers use
`media::encoder_timing`, `media::provider_encode_timing` or
`media::video_encode_timing` on a live, authenticated typed binding. These helpers
validate the optional version/size and snapshot bounds. Unsupported, pending or
busy queries yield no sample. `EncodeTimingSnapshotV1::record` and
`mean_ms_since` supply checked counter recording and interval conversion.

The reference encoder's backend packets retain an internal readiness timestamp.
Native sessions match it to the original input and count it once, including when
the output sink rejects the same packet repeatedly. Stream-session forwards
counters through the public stream snapshot and derives recorded interval means;
the stats plugin renders the same live and historical meaning. This does not
measure capture-to-display or client delivery.
