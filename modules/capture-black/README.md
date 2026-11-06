# Generated black capture

`amaterasu.capture-black/1` is an explicitly selected capture provider for streams
that need input/control without reading a desktop. It uses the public version 1.8
Capture SDK contract and exposes borrowed NV12 CPU resources. It creates no
threads, operating-system displays, audio routes or input devices.

Trusted startup composition enables it with `{"enabled":true}` and maps the
external video capture identity `black` to the service with
`frame_transport: "generic_cpu"`. There is no implicit fallback to this provider.

The requested output is `black:WIDTHxHEIGHT@FPS:nv12`, for example
`black:1920x1080@60:nv12`. Dimensions must be positive even values no greater than
8192 and FPS must be 1–240. The provider holds one immutable pixel allocation per
stream, delivers at the requested cadence, and advances its frame identity only
when the consumer accepts delivery. A rejected callback can retry the same frame.
Resource selection is bound to that stream's non-reused generation.

The shared CPU ABI currently supports NV12 and BGRA8 only. This provider emits
limited-range NV12 black; it does not advertise P010 or certify an encoder's
conversion to a requested ten-bit SDR/HDR output. That capability is negotiated
and verified by the selected encoder, separately from this source.

There are at most 32 active streams, 256 MiB of shared pixel storage, and 4096
active or recently retired operation records. Closed operations remain
idempotent for 30 minutes; overload fails explicitly. Stream IDs never repeat.
Close remains pending while a synchronous frame callback is active; lost open
completion rolls back only the stream created by that request.
