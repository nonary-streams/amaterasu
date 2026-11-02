# Capture provider SDK

This document describes the implemented native capture-to-encoder path. Native
providers are trusted code loaded into the user worker; runtime grants control
service routing, not a native-code sandbox. The media ABI is still draft.

## Publish frames and request a capture swap

The Rust SDK's `capture` module provides a capture-only channel. The capture
worker owns how pixels are acquired; publishing never invokes the encoder.
Each channel is one capture session, so a producer cannot select another
session by supplying an ID. The latest frame replaces the previous mailbox
frame, rather than accumulating a queue of old frames.

```rust,ignore
use amaterasu_module_sdk::capture::{capture_channel, SwapRequest};

let (publisher, frames) = capture_channel();
// Move publisher to the capture worker; retain frames in the provider adapter.
publisher.publish(my_owned_frame)?;

let request_id = publisher.request_capture_swap(SwapRequest {
    preferred_provider: Some("another_capture".into()), // or None: consumer chooses
    output: Some("matching-output-id".into()),
    reason: "source_unavailable".into(),
})?;
// Keep capturing if possible. Acceptance itself does not stop this provider.
let decision = publisher.swap_decision(request_id)?;
```

`publish` moves an owned frame without copying its pixels. `T` must be `Send +
Sync`: use `OwnedCpuFrame` for CPU pixels or an immutable GPU-resource owner
with a real cross-thread ownership contract. A borrowed DMA-BUF descriptor is
not an owner. Existing `RetainedDmaBufFrame` values remain confined to their
module thread; this API does not make them transferable between threads.
Platform-specific GPU resource owners still supply the acquire fence and obey
the existing leased-frame contract when exposing a frame to consumers.

The reader's `latest_frame()` acquires an `Arc<T>` snapshot; no mailbox lock is
held while a consumer reads it. The source retains one latest frame for repeated
reads. Readers must separately bound their in-flight frames and GPU leases.
For CPU providers, `CaptureFrames<OwnedCpuFrame>::read_cpu_frame(sink)` adapts
that snapshot to the existing borrowed native CPU callback without another
pixel copy. Authenticate the caller and stream before invoking the adapter.

`publisher.stop("source_lost")` reports a terminal capture failure and wakes
consumers even when no further frame can arrive. It is distinct from an advisory
swap request: video stops the failed stream instead of repeating stale pixels
indefinitely. Acquired GPU frames still retain their normal completion lifetime.

Publishing to a contended mailbox returns `PublishError { error: Busy, frame }`
without accepting the frame; the producer can reuse or retry that resource.
Old resources are released outside the mailbox lock on the publishing thread;
resource destructors must not wait for GPU completion if capture needs a
nonblocking publish. `frames.close()` (also performed on reader drop) stops
future publication and callbacks. It does not revoke already acquired frames
or claim that their GPU work has completed.

Local consumers call `frames.notifications().wait_timeout(...)` or `take()`.
Ready flags coalesce, survive notification-before-wait races, and wake waiters
without transferring pixels. The complete runnable example is
`crates/module-sdk/examples/capture_notifications.rs`:

```sh
cargo run --locked --offline -p amaterasu-module-sdk --example capture_notifications
```

### Native notification boundary

Capture v1.7 appends `subscribe_events`, `unsubscribe_events`, and `resolve_swap`
to the unchanged v1.6 prefix. Older CPU and GPU frame tables remain usable.
Native consumers must check both minor version and table size before using the
tail. Providers can expose v1.7 even when their GPU read callbacks return
`MEDIA_UNSUPPORTED`; notification support does not promise a GPU transport.

After authorizing its bound caller and stream, a provider delegates subscription
to `frames.subscribe_events(sink)` and swap replies to
`frames.resolve_native_swap(id, decision, reason)`. Unsubscribe delegates to
`frames.unsubscribe_events()`. The provider must track subscriptions owned by
each native binding and unsubscribe them before releasing that binding. The
SDK mailbox permits one native subscriber per capture stream. Registration
announces an already published frame and any pending swap, closing the startup
race. A failed subscribe must retain no callback/context.

`CaptureEventSink` callbacks may execute on a capture thread. They only record
bounded metadata and wake the receiver; they must not acquire frames, reenter
the provider, run encoding, or unwind across the ABI. Unsubscribe and control
close are synchronous notification lifetime barriers. All admitted callbacks
must finish before the consumer storage or provider code is retired.
The `stopped` callback carries terminal source loss independently of swap requests.

The SDK consumer wrapper `CaptureSubscription::subscribe(&binding, stream)`
pins that binding through unsubscribe. Its notification reader can wait on
another thread, while native frame acquisition and swap decisions stay on the
module thread. `CaptureNotifications::native_sink` is the lower-level adapter
for consumers already managing subscription/binding lifetimes.

Video subscribes to v1.7 streams and reads newly published frames only after a
ready notification, subject to the stream's existing frame-rate cap. Repeats,
keyframe requests and encoded-output draining retain their existing behavior.
Providers without the optional tail retain their existing pacing. The SDK's
notification wait is directly wakeable; video still consumes notifications on
the host's existing tick dispatcher. This change does not replace that host
scheduler or change encoder submission/synchronization.

### Swap request semantics

Requests are advisory and scoped to the current subscription. There is one
outstanding request, with a monotonically increasing nonzero ID. The provider
may keep publishing while the coordinator decides. A rejected request leaves
capture running. An accepted request approves a transition, not successful
startup of the replacement. Subsequent control close stops the old source.
The producer can read the decision until the next request or close. A native
request notification rejected with `MEDIA_BUSY` is retried by subsequent
publications or explicit `publisher.retry_notifications()`.

The preferred provider is an alias from trusted startup configuration, never a
service name or arbitrary executable. The consumer chooses if it is absent.
Output IDs are provider-specific; a coordinator must establish a mapping or
reject the request. Reasons are bounded identifier codes, not arbitrary logs.
Swap requests do not grant new service access or elevated permissions.

Video currently accepts transitions between configured CPU providers with an
explicit destination output and unchanged encoding configuration. It replies
before retiring the old subscription, confirms the old capture closed, and
opens the new source under a fresh operation identity. A rejected or failed
reply never starts a replacement. A replacement open failure uses the normal
stream cleanup path. GPU capture transitions requiring retirement/reopening
of encoder or GPU processor sessions return
`capture_swap_requires_pipeline_restart`; constant-rate mode transitions return
`capture_swap_requires_mode_restart`. Those downstream restart changes are
intentionally deferred. Requests with no output mapping return
`capture_swap_output_required`.

## Register a provider

A capture component registers a control service and a native capture table
under the same versioned service name. Its control API needs enumerate, open,
lookup and close with authenticated caller ownership and bounded cancellation
and cleanup. See `fixtures/capture-source` for a complete, independently named
test provider. The signed component set must include the provider, startup
dependency, and matching call and native-use grants for video. The launch
initiator also needs the call grant required for delegated capture open.

In video startup settings, register a bounded alias and select it at launch:

```json
{
  "enabled": true,
  "capture_providers": [],
  "external_capture_providers": {
    "example": "example.capture/1"
  }
}
```

Use `capture_provider: "example"` with the provider's output ID. The exact
service comes from startup policy, not the launch request. Unknown aliases and
duplicate service bindings are rejected. Registration does not install a
component, grant permissions, or authorize elevated operations.

For Linux DMA-BUF delivery, use an object instead of the CPU shorthand:

```json
{"example":{"service":"example.capture/1","frame_transport":"dmabuf"}}
```

The provider must implement capture v1.4 and the selected encoder must advertise
DMA-BUF input and implement encoder v1.1. Capture v1.5 optionally supplies
presentation-driven pacing. A pinned external DMA-BUF provider fails when the
required path is unavailable; it does not silently substitute another source.

## Negotiated resource delivery

Capture v1.8 adds an optional resource interface after the v1.7 table. Check
both the minor version and table size before using it. It does not change the
legacy CPU or DMA-BUF reads. Once the control service has opened and authorized
the stream, enumerate its resource offers, choose a compatible offer, and select
its ID with the enumeration generation. A stale or unsupported selection fails;
a capability change invalidates the selection and requires renegotiation.
Providers authenticate ownership on every operation. Descriptors returned during
enumeration are borrowed only for that callback: copy any data needed later.

Offers describe the resource and synchronization schemas and whether a frame is
borrowed only or retainable. Explicit CPU/GPU accessibility flags let callers
enforce their requirements even for a vendor schema; unspecified accessibility
is not proof of either. These flags describe access under that schema, not
physical residency or a no-copy promise. Standard CPU and DMA-BUF capability payloads specify
formats and size ranges; DMA-BUF adds modifier, color and device identity. A
zero device identity is unknown and does not establish compatibility. Custom
schemas use a nonzero vendor UUID, kind and version. Only components that agree
on a custom schema can interpret its payload. The SDK's `capture::select_delivery`
helpers select among bounded lists with ordered preferences and explicit
requirements; use schema-aware matching when capability equality alone does not
establish compatibility. Consumer configuration must still verify actual import
support.

`CaptureResourceFrame` carries common frame metadata plus a typed resource and
acquire synchronization descriptor. Optional color metadata is separate from
the resource. Frame and pacing timestamps have independent optional clock-domain
and epoch metadata; absent information means unknown, not an implied color space
or shared clock. CPU and DMA-BUF v1 frame payloads use the
existing `CpuFrame` and `DmaBufFrame` layouts, not the offer capability layouts.
Frame descriptors are borrowed for the callback. A successful retain pins the
resource, its immutable pixels and descriptor data until release. Borrowed-only
providers return unsupported from retain. The consumer waits for acquisition
and releases after all accesses complete, or supplies a supported completion
object covering pending work. The provider duplicates or retains completion
handles synchronously and quarantines storage if that fails. Bindings and
providers must remain alive until retained frames and pending fences retire;
closing a stream or changing its selection cannot invalidate a retained frame.
Retain/release are module-thread operations unless a schema explicitly provides
a stronger contract.

Rust consumers can use `BorrowedResourceFrame::copy_cpu` for an explicit
independent copy of a standard CPU frame. The returned `OwnedResourceCpuFrame`
retains its presentation, color and clock metadata as well as the copied pixels.

`FrameResourceConsumerApi` and `FrameResourceProcessorApi` define independent
consumer and processor tables for implementers. Configure the consumer against
the chosen producer offer before selecting the producer. If either step fails,
do not submit; re-enumerate and configure again. These ABI definitions alone do
not make every existing encoder or processor accept every resource. GPU-backed
delivery does not imply dedicated VRAM residency or an end-to-end zero-copy
path. CPU copies and GPU readback are explicit operations, not implied by a
resource offer.

The optional `capture::ResourceConnection` helper forwards resources to a
compatible native consumer without interpreting their payload. It can require
CPU and/or GPU accessibility and checks offer generations before selecting a
producer. Its consumer cancellation is a lifetime barrier: a stale configured
connection is retired, so reopen the consumer stream before creating another
connection. The helper does not open services or stream owners; its unsafe
constructor requires those bindings and owners to remain live.

For an external provider that implements v1.8, video can select a standard CPU
resource explicitly using `"frame_transport":"generic_cpu"` in its provider
registration. Video submits that frame synchronously through the existing CPU
encoder path. The bridge currently requires unknown color metadata and does not
use generic source timestamps for pacing. This setting does not provide generic GPU or custom-resource
forwarding to encoders; those require a compatible resource consumer and its
own binding and lifecycle integration. Existing `cpu` and `dmabuf` settings
continue to use their existing interfaces.

## Frame and processor contracts

The C declarations live in `sdk/include/amaterasu_media.h`; the Rust equivalents
are exported through `amaterasu_module_sdk::media_abi`. CPU frames are borrowed
only for the synchronous callback or submission. `BorrowedCpuFrame` validates a
CPU layout, and `OwnedCpuFrame` makes an independent copy for work beyond the
callback. Encoder success means it accepted/copied the input; busy accepts
nothing. No Rust allocation is transferred across the native ABI.

An optional `cpu_processor_service` in video startup settings selects a native
`CpuProcessorApi` provider. It receives a borrowed CPU frame and synchronously
delivers one output to the encoder sink. A successful output must preserve the
timestamp and sequence. Busy accepts nothing; malformed delivery or failure
fails the stream. The processor's output is copied for later repeats so repeats
do not process a frame twice. The configured component needs a native-use grant
and a signed startup dependency. This processor does not accept GPU frames.

The legacy capture v1.4 DMA-BUF descriptors remain borrowed for the callback.
Duplicating a descriptor pins its allocation but does not prevent the producer
from overwriting its pixels. A consumer must obey the acquire fence and finish a
safe copy before the producer can reuse that storage.

Capture v1.6 and encoder v1.2 add `DmaBufFrameLease`, which pins immutable pixels
as well as their allocation. The provider owns the initial reference during the
callback; consumers explicitly retain any reference needed afterward and release
it exactly once. Retention can return busy when the provider's finite limit is
reached. Release transfers a completion fence to the provider, or uses `-1` only
after the consumer's GPU work has completed. Invalid fences must quarantine the
storage. Acquire fences still apply. The provider's native binding must outlive
all retained references and pending fences. The additive entries are
`CaptureApiV1_6.read_dmabuf_frame_leased` and
`EncoderApiV1_2.submit_dmabuf_leased`. Ama's native encoder retains the lease
and queues GPU work without blocking the module tick. It releases the reference
on the module thread when encoded output proves completion, or after worker
shutdown joins outstanding work. Other encoders must obey the same lifetime
contract when retaining a lease.

Encoder v1.2 and GPU processor v1.0 must implement
`cancel_dmabuf_leases(context, stream)`. This is a synchronous lifetime barrier:
finish pending reads and retire every retained reference and completion fence
before returning. Video calls encoder cancellation before processor cancellation
on stream close and shutdown, while the source bindings remain alive. A provider
must not schedule later callbacks after cancellation. The built-in encoder keeps
at most one GPU submission in flight and does not queue older frames behind it.

Rust SDK users can retain a validated borrowed lease with
`RetainedDmaBufFrame::retain_from_borrowed(&lease, &native_binding)`. Its unsafe
constructor requires a trusted provider obeying the frame and fence contract.
The binding borrow prevents unloading while the retained frame exists;
`try_clone` can fail when retention is full. The wrapper is neither `Send` nor
`Sync`. Dropping it releases with `-1`, so callers must finish GPU work first, or
use the unsafe `release_after(OwnedFd)` method to transfer a completion fence.
The wrapper cannot infer that external GPU work has completed.

An optional `gpu_processor_service` selects GPU processor v1.0. It receives an
immutable borrowed input lease and synchronously delivers one output lease,
preserving sequence and timestamp. It may change output pixels and layout but
cannot mutate its input. A sink explicitly retains output needed after the
callback. This interface does not schedule asynchronous callbacks. It requires
the capture and encoder lease APIs and cannot be combined with
`cpu_processor_service`; a configured processor is never silently skipped.
Legacy providers retain their existing borrowed-frame route. See
`docs/media-interfaces.md` for format and clock details.

The [Processing SDK](gpu-processing-sdk.md) supplies host-invoked effect
factories, provider adapters and the same consumer dispatch used by video.
Processor v1.1 optionally describes processed HDR metadata independently of the
capture source. [RTX HDR](../modules/rtx-hdr/README.md) is a separately built
implementation selected through the existing GPU processor setting.

## Virtual-screen capture

`amaterasu.capture-vkms/1` is an independently built provider for the existing
`vibeshine_drm` virtual output path. It can also be registered as an external
alias when `vkms` is not simultaneously present in `capture_providers`:

```json
{
  "enabled": true,
  "capture_providers": [],
  "external_capture_providers": {
    "virtual_screen": {
      "service": "amaterasu.capture-vkms/1",
      "frame_transport": "dmabuf"
    }
  }
}
```

This requires the VKMS module, signed dependencies and grants, a compatible
encoder and a currently available owned `kwin:Virtual-N` output. The Linux
session profile generates the alias with `--enable-virtual-screen-capture` and
requires `--enable-capture` and `--enable-virtual-display` with the default capture
output.

Use `--capture-authority-helper ABSOLUTE_SOURCE` and optionally
`--capture-authority-card CARD` to package the helper and declare an exact signed
request for frame export. The generated provider selects `authority_channel:true`
and `gpu_snapshot:false`. The protected broker launches the approved unprivileged
worker and the host injects its private channel; user policy cannot name an
authoritative descriptor. Build capture-vkms with
`linux-kwin-geometry` for this mode. The default build uses the existing `WAIT_PRESENT` / `GET_FRAME` driver ABI
and advertises capture v1.5 borrowed DMA-BUF delivery. It does not require the
experimental driver lease ioctl. `linux-gpu-snapshot` and `gpu_snapshot:true`
opt into private GPU copies and retained-frame interfaces. Only building both
provider and helper with `driver-read-leases` requires the experimental leased
export; that mode fails explicitly if the matching driver is unavailable.
Unavailable approved authority always fails explicitly.

Private snapshots preserve supported 8-bit and 10-bit RGB storage; unsupported
imports or insufficient framebuffer precision fail instead of reducing precision.
Rendered pixel accuracy and multi-GPU operation still need hardware testing.
The guarded HDR path uses capture v1.9 metadata, preserves PQ/BT.2020 RGB10, and supplies HDR10 to the native NVENC/CUDA HEVC and AV1 encoders. See [HDR capture](hdr-capture.md) for its requirements and qualification boundary.

The older `--frame-helper ABSOLUTE_PATH` remains a mutually exclusive,
separately authorized compatibility mode. Package authority never falls back to
that mode. See the [privileged authority workflow](capture-plugin-privileged-authority.md)
for review, explicit administrator approval, protected deployment and revocation,
and [Linux session options](linux-session-profile.md) for profile configuration.
Generating or signing a profile does not install the helper or change a running
service.

Unit and simulated integration tests do not establish a real KWin, driver,
GPU-encoder or client playback result. Qualify those paths on the target host.

### Required GPU leases

The VKMS plugin accepts `require_lease:true` with `gpu_snapshot:true` when built
with `driver-read-leases,linux-kwin-geometry,linux-gpu-snapshot`; its helper must
also be built with `driver-read-leases`. This requests the new driver read guard,
waits for the GPU snapshot and releases its temporary source import, then lends
the independent immutable snapshot. This introduces one GPU capture copy and
performs no CPU pixel readback.

Set `require_frame_lease:true` in the video component as well. The video pipeline
then requires both capture and encoder lease interfaces and refuses borrowed or
CPU fallback on startup and runtime failures. The plugin independently refuses
its ordinary borrowed read callback when `require_lease` is set. A missing driver,
helper, GPU snapshot backend or encoder lease interface is a failure, not a
silently different delivery mode. The encoder retains accepted lease submissions
through GPU completion; cancellation joins that work before releasing references.
