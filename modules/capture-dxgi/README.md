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
  Enumeration is read-only and available when capture is disabled.
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
- Open accepts optional `operation_id` (1..128 UTF-8 bytes). Repeating the same
  caller/output/operation ID returns the existing live stream; changing output
  returns `operation_conflict`. `{"action":"lookup","operation_id":"start-1"}`
  recovers that caller's same `stream`/`output` reply after a lost control result.
  Closing or device loss removes the lookup mapping. Lookup and replay replies
  never mark a pre-existing stream as newly opened for completion-loss cleanup.
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
the control transport envelope; output enumeration pagination is not implemented.

## Native frame interface

Bind `CaptureApi` version 1.1 on the same service. Version 1.0 remains available
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

A successful read calls the sink once with tight BGRA8 pixels
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

## Windows implementation and remaining hardware gates

The Windows backend uses official `windows` 0.62 COM wrappers:
`CreateDXGIFactory1`/adapter-output enumeration, `D3D11CreateDevice` on the chosen
adapter, `IDXGIOutput1::DuplicateOutput`, and `AcquireNextFrame(0)`. A successful
acquisition is always paired with `ReleaseFrame`, including error/unwind paths.
The source is copied into a CPU-readable D3D11 staging texture. `Map` uses
`DO_NOT_WAIT`; pending staging textures remain owned until a later read.
`Unmap` is guarded on every path. Windows API/driver creation and submission calls
can still stall internally; nonblocking polling is not a hard real-time guarantee.

Current limitations are explicit:

- Rotated outputs are rejected with `unsupported_rotation`; no rotated pixels
  or metadata conversion is invented.
- Hardware cursor shape/position is not composed. DXGI may already include some
  cursor pixels, but a separate hardware cursor is absent.
- HDR preservation/tone mapping, dirty/move rectangles, protected-content
  diagnostics, zero-copy GPU frames, and GPU resource sharing are not implemented.
- CPU frames require a new staging texture and allocation per capture. This is a
  correctness path, not yet a benchmarked low-latency GPU pipeline.
- Linux/macOS report `unsupported_platform`; there is no fallback capture backend.

Tests use fake sessions and owned byte arrays only. They cover native binding
ownership/release, exact callback pixels, pending/busy/device-loss outcomes,
row-pitch copying, cancellation, capacity, and shutdown. Windows code and native
ABI compile for x86_64 MSVC. **No physical desktop capture has been run.** Manual
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
[ReleaseFrame](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-releaseframe).
