# Host-invoked GPU processing

The SDK follows the Hollywood principle: the framework calls an effect at the
processing stage. An effect implements `GpuFrameProcessor`; its
`process_to_resource` method receives a `GpuFrame` and returns an owned
`ResourceOutputFrame` or a status. It does not obtain
capture/video internals, pull frames, choose other plugins, or run an alternate
pipeline. The SDK invokes the downstream sink and propagates rejection.

`GpuProcessorFactory` creates independent state for each runtime-authenticated
consumer principal. Its factory closure is `Send + Sync`; each produced effect
is local to the consumer's module thread. Keep the boxed factory alive through
all bindings and register it through `GpuProcessorFactory::register`. Runtime
registration uses `amaterasu.gpu-frame-processor.native/1`, independently of the
service alias. Consumers select an alias in trusted startup composition with
explicit native-use grants and provider dependencies. Native plugins are
trusted code, and these grants are not a native-code sandbox.

Built-in video uses `negotiate_resource_handoff` and `process_gpu_to_resource`
to pass the selected processor's output into the encoder's resource API.
Negotiation is mandatory for a selected GPU processor: missing resource tables
or incompatible offers fail explicitly, while BUSY/PENDING defer capture and
retry. Video never substitutes the older processor-to-DMA-BUF output route.
External consumers use these same SDK adapters. They validate output identity,
synchronous single delivery and sink status, while `ScopedGpuFrame` bounds
temporary retained input references to the call.
The SDK provider adapter handles the native ABI, binding release, panic
containment, output delivery, metadata queries and cancellation dispatch.
Factories reject unsupported versions before invoking user code and never
replace a failed provider with another implementation.

The effect owns its GPU allocation and lifetime callbacks. Creating a
`ResourceOutputFrame` is an unsafe platform boundary: it transfers one real reference
whose storage and code must outlive every downstream retained reference and
completion fence. The SDK releases that initial reference after synchronous
delivery. The effect must finish input reads before returning (or independently
retain and fence its input according to the native contract), never mutate
input pixels, and bound its output storage. BUSY returns no delivery before
admission, and downstream rejection is returned unchanged.

`cancel` and `shutdown` are synchronous barriers. Callers retire downstream
consumers before the processor; a provider must not destroy referenced storage
or return a false GPU completion guarantee. A panic during either void barrier
aborts the worker. Source sequence, timestamp and freshness are preserved.
The input contract transports Linux DMA-BUF leases. The resource output contract
negotiates storage and synchronization schemas between processor and encoder;
video does not interpret vendor GPU handles or own their allocations.

## Additive output metadata

`GpuProcessorApiV1_1` preserves the entire v1.0 table and appends
`hdr_metadata(context, stream, output)`. Both minor >= 1 and sufficient size are
required before reading this tail. The query performs no GPU work:

| Result | Meaning |
| --- | --- |
| OK, `enabled=1` | HDR10 output with the returned metadata |
| OK, `enabled=0` | SDR output |
| UNSUPPORTED | Output metadata is unspecified |
| Other failure | Query failure; stop the stream |

SDK implementers override `GpuFrameProcessor::hdr_metadata`; the default is
unspecified. Video uses the video stream ID for the query, checks known output
range against the negotiated encoder stream before processing, and publishes
the processed metadata. It does not substitute capture metadata for transformed
output. The current video path requires the v1.4 resource-output table, which
includes this metadata operation. C providers use `AmaterasuGpuProcessorApiV1_1` and its matching size
constant from `sdk/include/amaterasu_media.h`.

## Stream settings and live updates

The v1.2 `configure_stream` operation supplies bounded JSON before processing.
The optional v1.3 `update_stream` operation replaces the full configuration
for an already configured stream. The provider owns the format and must copy
any data it needs after the call. SDK consumers use `update_gpu_stream` on a live,
typed GPU processor binding; the interface identity must be verified separately
from the table version and size. Older processors and providers that do not
implement updates return `MEDIA_UNSUPPORTED`.

Updates must be serialized with frame processing and stream retirement. A
rejected update leaves the previous configuration usable and does not cancel the
stream. Successful updates must preserve the negotiated output format and HDR
metadata; changes that cannot preserve them must be rejected. A settings save
or a newly observed revision only establishes that configuration was stored or
read. It does not establish that the processor accepted an update or that the
result reached a client display.

The versioned `processing_intent` API describes pipeline participation separately
from the effect's settings. Version 1.1 adds optional `output_hdr` to the existing
`target`, `revision`, and `settings` request. Trusted composition selects
`interface_minor: 1` in both the lifecycle `processing_policy` and stream-session
`processing_plan`; omitted versions remain 1.0 and receive the original request
shape. Consumers build either request with the shared SDK's
`processing_intent_payload` helper.

The selected provider's successful reply declares the source and output ranges
independently. For example, an SDR-to-HDR effect returns these fields alongside
the echoed target/revision, selected processor service, and processor settings:

```json
{"enabled":true,"source_hdr_required":false,"output_hdr_required":true}
```

The session provider uses that frozen source requirement when creating its
owned virtual display, including a shared source retained for additional
clients. Video and the encoder retain the negotiated output range. Joining
clients reuse the source's recorded range; their output requests do not
reconfigure it. With processing bypassed, capture follows the output range.
Providers declare requirements through this contract; they do not need display
provider identities, OS handles, or direct display mutation. The same unchanged
session consumer accepts independently selected processing providers and their
different source-range requirements.

For an HDR output, a provider can return `enabled: true` to reserve its processing
path while its own effect switch is false. It must still supply matching source
and output range requirements and preserve the frozen settings. The source is
selected before capture begins. Live effect changes then use `update_gpu_stream`
without changing negotiated formats or replacing the stream. For example, RTX
HDR retains an SDR source and PQ output while its disabled effect maps SDR into
that output; its enable option is a `live_action` at all supported scopes.

The [RTX HDR module](../modules/rtx-hdr/README.md) implements this interface using
NVIDIA TrueHDR. The SDK's replacement test runs an unchanged consumer with two
independent effect types, including sink rejection, retained output and teardown.
