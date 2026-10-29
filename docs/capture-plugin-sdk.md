# Capture provider SDK

This document describes the implemented native capture-to-encoder path. Native
providers are trusted code loaded into the user worker; runtime grants control
service routing, not a native-code sandbox. The media ABI is still draft.

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
`EncoderApiV1_2.submit_dmabuf_leased`. Ama's native encoder waits for its GPU
reads to complete before returning; a third-party encoder may retain a lease
until its own reads complete, provided it obeys that lifetime contract.

Encoder v1.2 and GPU processor v1.0 must implement
`cancel_dmabuf_leases(context, stream)`. This is a synchronous lifetime barrier:
finish pending reads and retire every retained reference and completion fence
before returning. Video calls encoder cancellation before processor cancellation
on stream close and shutdown, while the source bindings remain alive. A provider
must not schedule later callbacks after cancellation. The built-in encoder's
synchronous source-copy barrier can block the module tick during GPU work.

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
and `gpu_snapshot:true`. The protected broker launches the approved unprivileged
worker and the host injects its private channel; user policy cannot name an
authoritative descriptor. Build capture-vkms with
`linux-kwin-geometry,linux-gpu-snapshot` for this mode. Its snapshot implementation
must preserve immutable frame contents, rather than merely duplicate mutable
scanout descriptors. Missing snapshot support or unavailable approved authority
fails explicitly.

Private snapshots preserve supported 8-bit and 10-bit RGB storage; unsupported
imports or insufficient framebuffer precision fail instead of reducing precision.
Rendered pixel accuracy and multi-GPU operation still need hardware testing.
PQ/BT.2020 HDR signaling remains unsupported in this KMS pipeline.

The older `--frame-helper ABSOLUTE_PATH` remains a mutually exclusive,
separately authorized compatibility mode. Package authority never falls back to
that mode. See the [privileged authority workflow](capture-plugin-privileged-authority.md)
for review, explicit administrator approval, protected deployment and revocation,
and [Linux session options](linux-session-profile.md) for profile configuration.
Generating or signing a profile does not install the helper or change a running
service.

Unit and simulated integration tests do not establish a real KWin, driver,
GPU-encoder or client playback result. Qualify those paths on the target host.
