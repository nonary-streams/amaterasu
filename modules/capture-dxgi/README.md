# DXGI desktop capture

`amaterasu-capture-dxgi` is an independently built Rust `cdylib`/`rlib` provider
for `amaterasu.capture-dxgi/1`. It registers both JSON control and a native
`amaterasu_media::CaptureApi` on that service. Startup settings are
`{"enabled":false}` by default. The provider needs a service registration grant;
consumers need the corresponding service call and native binding grants.
Native modules are trusted code; these grants are not a native-code sandbox.

## Control API

- `{"action":"status"}` returns `ok`, `enabled`, `native_supported`, and only
  the caller's `streams` (`stream` ID and `output` identity).
- `{"action":"enumerate"}` returns `outputs`: `id`, `adapter`, `device_name`,
  `attached`, `left`, `top`, `width`, `height`, and the DXGI numeric `rotation`.
  Enumeration is read-only and available when capture is disabled. Outputs are
  ordered by ID. Optional `offset` (default 0) and `limit` (1..=256, default
  256) page results. A `next_offset` field is returned when more outputs remain;
  each page fits the 12 KB control reply limit. Every reply includes an opaque
  `enumeration_id` for the complete sorted output list. Consumers should reject
  pages with a different ID and re-enumerate from zero after a topology change.
- `{"action":"open","output":"<enumerated id>"}` returns
  `{"ok":true,"stream":123,"output":"..."}` plus `desktop_geometry` when
  Windows can prove the selected monitor's physical rectangle and the complete
  virtual desktop. A separate `touch_surface` object contains positive
  `width`, `height`, `logical_width`, and `logical_height` when DXGI, GDI, and
  CCD verify the same physical output, its dimensions agree with the advertised
  output rectangle, and a second geometry check confirms that the output did
  not move during open. Its logical dimensions are physical Windows pointer
  units; no stream letterboxing is claimed. The field is omitted if geometry
  or that mapping cannot be verified. IDs are opaque JSON unsigned 64-bit
  integers; clients must preserve integer precision.
  Rotated outputs use the desktop rectangle's oriented dimensions for both
  frames and `touch_surface`; a changed mode or target closes the stream.
- Open accepts optional `operation_id` (1..128 UTF-8 bytes). Repeating the same
  caller/output/source-format/operation ID returns the existing live stream;
  changing output or source format returns `operation_conflict`. `{"action":"lookup","operation_id":"start-1"}`
  recovers that caller's same `stream`/`output` reply after a lost control result.
  Closing or device loss removes the lookup mapping. Lookup and replay replies
  never mark a pre-existing stream as newly opened for completion-loss cleanup.
- Open accepts `source_format:{"bit_depth":10,"dynamic_range":"sdr"}` or
  `"hdr"`, independently of the encoder codec. Explicit eight-bit SDR is also
  supported; HDR requires ten bits. A source-format request requires the public
  preflight capability `format_open:true`; older providers receive unchanged
  Open bodies. Explicit formats verify the exact native desktop's dynamic range
  before capture allocation and during delivery. A range mismatch requires
  explicit processing rather than silently clipping HDR. Omitting the field
  preserves the legacy BGRA8 capture path.
- `{"action":"close","stream":123}` closes only a stream owned by the
  runtime-authenticated caller. Unknown and foreign IDs both return `not_found`.

Errors use `{"ok":false,"error":{"code":"...","message":"..."}}`.
At most four streams are open per module. Opening checks runtime request activity
before and after native resource creation; cancellation drops the newly created
session. A lost successful open reply also closes that specific new stream.
Shutdown closes all streams. Releasing a native binding releases its context;
call `close` to close a stream independently of binding lifetime.

An output ID combines the adapter LUID with the GDI display name. It identifies
an output in the current Windows topology; it is **not** a durable monitor serial
or an identity guaranteed across restart, adapter replacement, or topology changes.
Opening re-enumerates and matches the exact output. There is no automatic selection
of another display. Enumeration is bounded to 32 adapters and 64 total outputs. Metadata replies
exceeding 12,000 encoded bytes return `resource_exhausted` rather than exceeding
the control transport envelope.

## Native frame interface

Bind the public capture family on the same service. Version 1.0 remains available
as the unchanged prefix for older consumers. The 1.1 `geometry` callback returns
the open stream's validated `CaptureGeometry`, or `MEDIA_UNSUPPORTED` when open
could not prove geometry. Once geometry was advertised, a changed rectangle,
missing display, or changed physical target closes the stream rather than
returning stale coordinates. A successful frame read also rechecks that complete
geometry before calling the pixel sink; a moved output closes the stream even
when its pixel dimensions stay the same. If Open could not prove geometry,
frame capture remains available without a touch mapping. `MEDIA_BUSY` means
another controller operation is in progress and the caller may retry without
consuming a frame.

The broker supplies the
immediate authenticated consumer identity at bind time. The provider copies it
into the binding; `read_frame(context, stream_id, sink)` accepts no caller string.
Every read checks stream ownership, including calls through another binding.
The binding context retains shared state and serializes access with a mutex.
Concurrent or reentrant reads return `MEDIA_BUSY` instead of blocking on that mutex.
The mutex is released before the consumer callback runs.

A successful legacy SDR read calls the sink once with tight BGRA8 pixels
(`stride = width * 4`, `len = stride * height`). Only initialized pixel bytes are
copied; GPU row padding is never included in a Rust slice or exported. The frame
and its bytes are borrowed only during that synchronous callback. Consumers must
copy or consume them before returning. The sink's status is returned unchanged;
a rejected frame is consumed, so it is not retried. No frame bytes travel in JSON.

`timestamp_us` is the monotonic elapsed time at readback submission from this
stream's creation epoch. It is not a wall-clock or DXGI presentation timestamp.
`sequence` increases for delivered CPU frames. Dimensions are at most 8192 on each
axis, with at most 128 MiB of tightly packed pixels per frame.

- `MEDIA_PENDING`: no desktop update or GPU copy is still pending; no callback.
- `MEDIA_BUSY`: another operation holds the controller; no callback.
- `MEDIA_CLOSED`: unknown/foreign stream, access lost, or device lost; no callback.
- `MEDIA_UNSUPPORTED`: unsupported native frame format/platform.
- `MEDIA_BACKEND_ERROR`: other native failure.

Access/device loss tears down the stream immediately. The consumer must
re-enumerate and explicitly open the desired output again. There is no hidden
reopen or fallback to a different monitor.

Geometry is accepted only when GDI, DXGI, and the active CCD topology report the
same complete attached-display set, rectangles, and monitor handles. The query
also retains the CCD target device-interface identity so a reused display name
cannot silently select a different physical target. Coordinate-producing calls
run under a scoped per-monitor-v2 thread DPI context and restore the previous
context before returning. If any source is inconsistent or unavailable, capture
may continue without `desktop_geometry`; absolute pointer input remains disabled.
Topology can still change immediately after the last validation, so the mapping
is revalidated before each delivered frame and is not an atomic snapshot with a frame.

The shared SDK `CpuSourceFactory` publishes capture v1.10. Its v1.8 resource
callbacks enumerate an exact nonzero generation and two P010 offers for a
high-depth source, with distinct full/limited-range IDs. CPU schema v2 includes
known color in negotiation; delivered frames carry the identical color tuple.
SDR uses BT.709 primaries/transfer/matrix; HDR uses BT.2020/PQ/BT.2020 NCL. P010
stores all ten meaningful bits in the high bits of little-endian words. The
v1.10 provenance callback authenticates the owner and guarantees that source
color is checked for every delivered presentation. A native range or precision
change stops delivery. The v1.9 HDR-enabled observation is native-derived;
unknown content mastering fields remain zero rather than copying display
capabilities into content metadata.

These are synchronous borrowed CPU resources. Retain returns UNSUPPORTED;
consumers copy with the same public SDK adapter used for other CPU providers.
Close retires the native session; an independent consumer copy remains valid.
Each binding stores at most 64 selections, each with at most eight format offers.
Stale generations or changed descriptions require explicit renegotiation.
The optional v1.11 wrapper performs bounded, read-only source preflight before
any capture allocation; unavailable targets never select another output.

## Windows implementation and remaining hardware gates

The host shutdown announcement rejects new opens while preserving existing
captures for consumers to drain. Provider quiescence then destroys all owned
duplication sessions before reporting ready, keeping its declared dependencies
alive until capture retirement. Portable fixtures verify this module lifecycle; COM
release timing and physical Windows shutdown still require native qualification.

The Windows backend uses official `windows` 0.62 COM wrappers:
`CreateDXGIFactory1`/adapter-output enumeration, `D3D11CreateDevice` on the chosen
adapter, `IDXGIOutput1::DuplicateOutput` for legacy BGRA8 and
`IDXGIOutput5::DuplicateOutput1` with only FP16 accepted for explicit
high-depth capture, and `AcquireNextFrame(0)`. A successful
acquisition is always paired with `ReleaseFrame`, including error/unwind paths.
The source is copied into a CPU-readable D3D11 staging texture. `Map` uses
`DO_NOT_WAIT`; pending staging textures remain owned until a later read. One
completed staging texture is retained and reused for compatible frames.
`Unmap` is guarded on every path. Windows API/driver creation and submission calls
can still stall internally; nonblocking polling is not a hard real-time guarantee.

Current limitations are explicit:

- Rotated outputs are returned in the output rectangle's orientation. The
  unrotated duplication texture is checked against that rectangle and its
  DXGI rotation, then copied into tight BGRA rows. An unknown rotation returns
  `unsupported_rotation`; a mode/dimension change closes the stream.
- A separate DXGI hardware cursor is composed into returned BGRA frames when
  `PointerPosition.Visible` is true. Color, masked-color, and monochrome shapes
  are cached across move-only frames, clipped to the output, and captured with
  the staging texture for delayed CPU readback. High-depth frames compose only
  cursor-covered pixels in the original source encoding; uncovered HDR values
  retain their original words. HDR cursor white uses the exact active CCD
  target's observed SDR reference-white multiplier, sampled with the pending
  frame; an unavailable or ambiguous target does not acquire a guessed value.
  Hidden or desktop-embedded
  pointers are not drawn again. Physical cursor behavior still needs hardware
  validation, including rotated monitors and driver-specific shape formats.
- High-depth capture requests FP16 scRGB and preserves its precision through
  conversion to public P010 resources. DXGI converts an original fullscreen
  backbuffer into that explicit format; the provider does not infer an original
  RGB10 application's transfer from scan-out HDR. Native Output6 color and
  actual duplication texture precision must agree; display HDR alone cannot
  establish source format. scRGB HDR uses Windows' 80-nit linear reference and is converted
  to PQ/BT.2020; ten-bit SDR is converted independently using BT.709. HDR-to-SDR
  tone mapping is an explicitly selected processor, outside this provider.
- Dirty/move rectangles, protected-content diagnostics, zero-copy GPU frames,
  and GPU resource sharing are not implemented.
- CPU frames reuse one compatible staging texture after readback. Mode changes,
  readback errors, and stream teardown discard it. This remains an unbenchmarked
  CPU readback path.
- Linux/macOS report `unsupported_platform`; there is no fallback capture backend.

Tests use fake sessions and owned byte arrays only. They cover rotation through
90, 180, and 270 degrees with non-square, padded surfaces; native binding
ownership/release, exact callback pixels, pending/busy/device-loss outcomes,
row-pitch copying, cursor shape composition and movement, cancellation, capacity,
and shutdown. Public-resource replacement tests cover ten-bit SDR/HDR color,
range negotiation, retained precision, close, native color loss, and consumer
copies surviving source retirement. The new Windows branches have not been
compiled in the Linux-only environment used for this increment; Windows target
standard libraries are unavailable. **No physical desktop capture has been run.** Manual
Windows validation still needs adapter selection, sustained frames, monitor mode
switch/access loss, device removal, cursor behavior, protected content, and
resource/memory measurements. Enable capture only for an explicitly requested
hardware session; normal tests never create a D3D device or duplicate a desktop.

Behavioral reference: sibling Vibeshine capture identity and snapshot/release
interfaces in `src/platform/windows/display.h`, and display history including
`a8af9679d` (automatic capture identity). Implementation was written independently.
Primary API references:
[desktop duplication](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api),
[AcquireNextFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe),
[ReleaseFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-releaseframe),
[DuplicateOutput1](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1),
[Windows HDR/scRGB](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range),
[DXGI color spaces](https://learn.microsoft.com/en-us/windows/win32/api/dxgicommon/ne-dxgicommon-dxgi_color_space_type),
[SDR white level](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-displayconfig_sdr_white_level).
