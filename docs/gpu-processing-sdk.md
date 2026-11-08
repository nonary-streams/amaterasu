# Capture-independent frame processing

Effects implement the public `FrameProcessor` trait and register a
`FrameProcessorFactory` under `amaterasu.frame-processor.native/1`. C providers
use `AmaterasuFrameProcessorApi`. The service alias chooses a provider; the exact
native contract identifies its table. The superseded GPU-only processor API is
removed. Rebuild providers and consumers together; there is no compatibility
route.

The framework selects and invokes an effect. Effects do not pull capture frames,
choose other plugins, or replace the host pipeline. Native plugins are trusted
code; grants select and authorize them rather than sandboxing native execution.

## Negotiation and delivery

`FrameProcessingSession` connects capture resources, up to 16 processor stages,
and a resource consumer. `FrameProcessingStage` accepts the common
`FrameResourceProcessorApi` prefix, including typed CPU resource processors.
Built-in video uses this same session and `EncoderResourceConsumer`; external
consumers have the same adapters. Bind every table under its authenticated
contract and keep every owner loaded through ordered cancellation.

Negotiate source to first processor before asking that processor for outputs.
For each edge, the consumer validates the producer's actual format, dimensions,
device, accessibility, ownership and synchronization schemas. Configure the
consumer, then select that exact producer generation and offer. Each provider
owns its imports and allocations. The SDK interprets no platform handles and
provides no implicit format conversion. Unsupported combinations fail explicitly.

A pump immediately relays one borrowed `CaptureResourceFrame` through the bounded
chain. It adds no pixel copy, frame queue or GPU completion wait. BUSY/PENDING
consume no queued SDK frame; the next pump reads the current source. Source
sequence, timestamp, capture pacing timestamp, both clock identities and freshness
survive every processing step. Only the final encoder adapter assigns the encoded
stream's timeline and submission sequence.

## Shared ownership and readiness

`ProcessingFrame::retain` acquires an independent provider reference.
`ProcessingFrame::forward` offers unchanged storage and readiness under the
selected output identity, using the original owner's lifetime callbacks. Additional
readers retain the same immutable storage. Sharing a descriptor never proves that
pixels are ready: queue the exact negotiated acquire dependency before access.

A provider returns `ResourceOutputFrame` by transferring one real reference through
its documented unsafe platform boundary. Its payloads and callbacks remain valid
while referenced. The SDK releases the initial reference after the sink returns;
asynchronous readers retain their own. Each reader releases after its own reads
finish, or supplies its own negotiated completion dependency. Never overwrite or
free storage merely because a different reader finished. Abandoning a retained
input without proving completion pins it rather than authorizing early reuse.

`CpuPixels` wraps capture-owned immutable storage without copying bytes.
`CpuFrameLeasePool` bounds outstanding references and bytes (defaults: 64 references,
256 MiB); exhaustion returns BUSY. `CpuSourceFactory::retirement_ready` keeps source
modules loaded until their retained resources retire. CPU transforms expose their
owned result through the same pool, allowing subsequent asynchronous GPU steps.
Unknown completion objects quarantine their storage and block retirement.
`CpuPresentationReceipt` acknowledges the synchronous callback separately, so a
retained GPU reader never holds a source mailbox admission gate.

## Cancellation and controls

The SDK cancels the final consumer first, then processors in reverse order. Every
provider's cancel/shutdown barrier retires its queued native work. A failed void
barrier aborts rather than falsely proving completion. Frame bindings, source
storage owners and provider libraries outlive the barrier and outstanding references.
Replacing an encoder retires the old reader and renegotiates only the final edge;
it does not retire the processor stream. Stream and graph admission are bounded.

`configure_frame_stream` and `update_frame_stream` use bounded JSON settings.
Updates reject atomically and preserve active settings on failure. HDR metadata
belongs to the processor's output and is independent of source layout or storage
depth. Ten-bit SDR and ten-bit HDR remain distinct supported source modes.

## RTX HDR and capture adoption

RTX HDR imports packed SDR DMA-BUF formats at eight or ten bits and explicit SDR
CPU BGRA8, NV12 and P010 resources. CPU uploads and format conversion belong to
that provider; their cost is not a zero-copy guarantee. It publishes CUDA output
with a readiness event, retains inputs through queued reads, and bounds both output
slots and in-flight reads. CUDA storage and reader references also have provider-wide
budgets. Independent readers keep their output and native session valid across
processing-stream cancellation; module retirement waits for them. Unknown or
contradictory source color is rejected.

VKMS snapshot capture queues source acquire fences on the GPU and exports the
immutable copy with its completion fence. It retains source imports and driver
read guards until that copy completes. The ordinary path contains no `glFinish`
barrier; native fence support is required. PipeWire coalesces a busy sink's stale
presentation to the newest available frame. CPU capture adapters share their
existing owned buffers through the resource SDK.

Source tests cover multiple schemas, multiple steps/readers, independent completion,
backpressure, stale generations, consumer replacement and bounded retirement.
Opt-in RTX fixtures cover real GPU output, one-slot pressure and CPU import modes.
These checks do not establish game-to-photon latency or a comparative frame delta;
that requires an installed candidate and equivalent live capture/encode/client
conditions. Processor callback CPU time excludes nested capture/encoder work.
GL sampling and CUDA pipeline timings are reported separately, without
claiming an end-to-end total.

## Lifecycle participation and source range

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
selected before capture begins. Live effect changes then use `update_frame_stream`
without changing negotiated formats or replacing the stream. For example, RTX
HDR retains an SDR source and PQ output while its disabled effect maps SDR into
that output; its enable option is a `live_action` at all supported scopes.

The [RTX HDR module](../modules/rtx-hdr/README.md) implements this interface using
NVIDIA TrueHDR. The SDK replacement tests cover independently implemented processors, sink
backpressure, retained output and ordered teardown.
