# Generated black capture

`amaterasu.capture-black/1` is an explicitly selected capture provider for streams
that need input/control without reading a desktop. It uses the public version 1.8
Capture SDK contract and exposes borrowed NV12 or P010 CPU resources. It creates no
threads, operating-system displays, audio routes or input devices.

Trusted startup composition enables it with `{"enabled":true}` and maps the
external video capture identity `black` to the service with
`frame_transport: "generic_cpu"`. There is no implicit fallback to this provider.

The requested output is `black:WIDTHxHEIGHT@FPS:FORMAT`, for example
`black:1920x1080@60:nv12` or `black:1920x1080@60:p010`.
Dimensions must be positive even values no greater than
8192 and FPS must be 1–240. The provider holds one immutable pixel allocation per
stream, delivers at the requested cadence, and advances its frame identity only
when the consumer accepts delivery. A rejected callback can retry the same frame.
Resource selection is bound to that stream's non-reused generation.

NV12 uses limited-range neutral codes Y=16, UV=128. P010 stores little-endian
ten-bit neutral black Y=64, UV=512 in the high ten sample bits. These neutral
values work for limited-range ten-bit SDR and HDR without reading or converting
desktop pixels. Storage format does not imply primaries/transfer; encoded color
and supported bit depth are negotiated by the selected encoder. P010 preserves
the existing native ABI layout and uses an explicitly advertised pixel format.
The output descriptor expresses the initial CPU format. Capture resource
enumeration offers both NV12 and P010 where their bounded layouts fit. The
unchanged generic video consumer selects the format matching its final encoded
bit depth, so an eight-bit preference can become P010 after ten-bit SDR is
negotiated. Selection waits for an in-flight callback and atomically replaces
only this stream's storage. Source tests do not certify hardware encoding or
client HDR presentation.

There are at most 32 active streams, 256 MiB of shared pixel storage, and 4096
active or recently retired operation records. Closed operations remain
idempotent for 30 minutes; overload fails explicitly. Stream IDs never repeat.
Close remains pending while a synchronous frame callback is active; lost open
completion rolls back only the stream created by that request.
