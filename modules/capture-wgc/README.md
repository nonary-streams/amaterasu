# Windows Graphics Capture provider

`amaterasu-capture-wgc` is an independently built Rust `cdylib`/`rlib` implementing
JSON control and the native `amaterasu_media::CaptureApi` on
`amaterasu.capture-wgc/1`. It can replace the DXGI capture provider through the
video coordinator's explicit provider selection. It does not wrap DXGI desktop
duplication or silently fall back to it.

Startup settings default to `{"enabled":false}`. Optional
`"capture_cursor":true` or `false` requests that WGC include or exclude its cursor.
When absent, WGC's default is retained. An explicitly requested cursor setting
that the OS does not support fails open with `unsupported_cursor`; it is never
silently ignored. The Windows capture indicator/border remains under OS policy.
The provider does not request border suppression or bypass desktop permissions.

The provider needs service registration; consumers need the matching service-call
and native-binding grants. Native modules remain trusted code, not sandboxed by
these grants. Linux/macOS implement explicit `unsupported_platform` responses.

## Control contract

- `{"action":"enumerate"}` returns `{"ok":true,"outputs":[...]}`. Each entry
  has `id`, `adapter`, `device_name`, `attached`, `left`, `top`, `width`, `height`,
  and `rotation`. Enumeration is read-only and works while capture is disabled.
  Entries are ordered by output ID. Optional `offset` (default `0`) and `limit`
  (default `256`, valid range `1..=256`) select a page, for example
  `{"action":"enumerate","offset":0,"limit":16}`. A reply with more outputs
  includes numeric `next_offset`; send it as the next request's `offset` until
  the field is absent. A no-argument request retains the original reply shape
  when the complete output list fits in one reply, plus `enumeration_id`.
  Every reply includes this 64-character SHA-256 hex digest of the complete
  sorted output metadata. If it changes between pages, restart enumeration.
- `{"action":"open","output":"<enumerated id>"}` returns
  `{"ok":true,"stream":123,"output":"..."}` plus `desktop_geometry` when
  Windows can prove the selected monitor's physical rectangle and the complete
  virtual desktop. A separate `touch_surface` object contains positive
  `width`, `height`, `logical_width`, and `logical_height` when the physical
  output topology is verified, its dimensions match the advertised output
  rectangle, and a second geometry check confirms that the output did not move
  during open. Its logical dimensions are physical Windows pointer units; no
  stream letterboxing is claimed. The field is omitted when geometry or the
  mapping cannot be verified. The opaque unsigned 64-bit stream ID must retain
  integer precision in clients.
- Open also accepts optional `constant_rate`; when `true`, it enables cached
  image repeats as described below. Omission and `false` retain event-driven
  delivery.
- Open accepts optional `source_format:{"bit_depth":10,"dynamic_range":"sdr"}`
  or `"hdr"`. This describes capture pixels independently of the encoder codec
  and output range. Omission preserves eight-bit SDR BGRA delivery. Ten-bit SDR
  and HDR both require an FP16 scRGB WGC frame pool; failure never retries BGRA8.
  Even dimensions and bounded CPU storage are required for P010 delivery.
  Explicit format requests require the exact native monitor's observed scan-out range
  to match. A different encoder output range requires explicitly selected
  processing; this provider does not tone map HDR into SDR.
- Open accepts optional `operation_id` containing 1..128 UTF-8 bytes. Repeating
  the same caller/output/delivery mode/operation ID returns the existing live
  stream. A different output, delivery mode, or source format with the same operation ID
  returns `operation_conflict`.
- Open accepts optional `constant_rate`, defaulting to `false`. When enabled,
  reads repeat the last completed WGC image if no newer frame is ready. The
  caller controls the read cadence; WGC does not start a timer. Repeated frames
  share the retained owned pixel allocation and receive new sequence numbers
  and monotonic timestamps. A read or geometry error, or a rejected sink
  delivery other than temporary `MEDIA_BUSY`, clears the cached image; closed
  or failed streams never replay it.
- `{"action":"lookup","operation_id":"start-1"}` recovers that caller's
  same stream/output reply after a lost open result. The mapping ends when the
  stream closes or loses its capture source.
- `{"action":"close","stream":123}` closes only the caller's stream. Its broker
  request remains pending until the worker has joined and the complete native
  session has been destroyed; only then does it return `{"ok":true}`. Other
  requests continue receiving ticks while the worker retires. Duplicate Close
  requests share that retirement. There are at most 32 retained Close waiters
  and queued cleanup targets; additional requests return `resource_exhausted`.
- `{"action":"status"}` reports `ok`, `enabled`, `native_supported`, and only
  that caller's `streams` (`stream` and `output`). `native_supported` means this
  build includes Windows code, not that this OS/session passed an open test.

Errors use `{"ok":false,"error":{"code":"...","message":"..."}}`.
There are at most four streams per module, 64 enumerated monitors, and 12,000
encoded bytes per enumeration reply, including `enumeration_id`. A page may
contain fewer than `limit` outputs to stay within that byte bound. A no-argument
request returns the first
page when the complete reply exceeds the bound. An offset at or beyond the end
of a nonempty list, or a limit outside `1..=256`, returns `invalid_request`;
offset `0` is valid for an empty list. If one output cannot fit, enumeration
returns `resource_exhausted`.

Monitor IDs are current GDI device names, not stable monitor serial numbers.
`adapter` is `system-selected`: WGC uses a system-selected D3D11 GPU, while the
monitor itself is always explicitly selected. WGC captures the monitor's
presented orientation, so `rotation` is identity (`1`) for the delivered frame.
Monitor IDs and GPU metadata are not interchangeable with DXGI output IDs.

The worker re-enumerates the exact requested ID when opening. An unavailable
monitor returns `not_found`; it never chooses a default or another monitor.
Closure, device loss, or changed content dimensions ends the stream. The caller
must re-enumerate and explicitly reopen the intended monitor; there is no hidden
reconnection or output substitution. Caller-scoped lookup cannot recover an ID
that was already closed, and restart does not preserve streams.

Opening checks runtime request activity before and after resource creation.
Cancellation retires the new session; losing its completion acknowledgment also
retires that specific newly created stream. Replay and lookup replies never
mark existing streams for this cleanup. Module shutdown closes every stream.
Native binding release releases that binding's context; use `close` to end the
stream separately.

Once Close has been admitted, cancelling its broker request releases only the
waiter. The exact caller/stream target remains owned until it can enter cleanup,
including while a native callback holds the controller. A closing stream rejects
new reads and replayed opens, retains its owner-scoped operation lookup, and
counts against stream capacity until destruction finishes. Terminal native
errors use this same retirement path. `MEDIA_CLOSED` ends frame delivery; a
successful control Close is the separate proof that native retirement completed.

The host shutdown announcement rejects new opens without retiring captures still
used by consumers. Once those consumers have quiesced, provider quiescence
disconnects frame requests and polls each owned worker. It reports ready only
after every worker has finished destroying its source and has been joined,
keeping its declared dependencies alive until capture retirement completes.
A worker panic remains a shutdown error on retries. These transitions are
covered by portable module and held-worker fixtures; native Windows shutdown
timing and driver behavior still require Windows qualification.

## Native frames and lifetimes

The provider publishes the public CPU capture resource API through version 1.10,
wrapped by the read-only version 1.11 source-preflight contract on
`amaterasu.capture-wgc/1`. The same SDK factory is available to external
providers. Legacy prefixes remain usable for eight-bit SDR streams; their
BGRA callbacks return `MEDIA_UNSUPPORTED` for a ten-bit stream.

Ten-bit streams offer P010 in explicit limited and full ranges, with BT.709 SDR
or PQ/BT.2020 HDR metadata. Conversion starts from the owned FP16 pixels for each
selected range, without an intermediate eight-bit image. The provider matches
GDI/CCD target identity and geometry to the exact DXGI Output6 color observation;
unknown scan-out encoding, insufficient HDR precision, or a range change stops
high-depth delivery. The native texture descriptor must match the selected
frame-pool format on every readback. Source range and geometry are checked again
before publishing fresh or cached frames and resource descriptions. HDR static
mastering fields remain zero when the source does not supply them.

WGC's FP16 path follows Microsoft's
[screen capture guidance](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture)
and [scRGB color model](https://learn.microsoft.com/en-us/windows/win32/direct3darticles/high-dynamic-range).
Pure tests cover both ranges, precision-preserving source layouts, caller
isolation, resource negotiation and retirement. Native Windows compilation,
actual Output6 observations, HDR playback and GPU performance still require
qualification on a Windows host.

 The 1.1 `geometry` callback returns the validated open-stream
geometry, or `MEDIA_UNSUPPORTED` when open could not prove geometry. Once
geometry was advertised, a changed rectangle, missing display, or changed
physical target closes the stream. Fresh frames and constant-rate cached repeats
recheck that geometry before pixel delivery, including moves that preserve the
capture dimensions. If Open could not prove geometry, capture remains available
without a touch mapping. `MEDIA_BUSY` reports temporary controller contention
and may be retried without consuming a frame.

The v1.2 freshness callback reports whether delivered pixels came from a fresh
WGC frame or cached reuse. The v1.3 `read_frame_with_capture_timestamp` callback
also returns a source pacing timestamp separately from `CpuFrame.timestamp_us`.
On Windows, `Direct3D11CaptureFrame.SystemRelativeTime` identifies when the
compositor rendered the frame. Its QPC time is mapped to this stream's epoch
before GPU readback. Missing, zero, regressing, future, or unrepresentable
source times fall back to the worker's local frame observation time; that
fallback cannot recover time spent queued by WGC. The controller keeps its own
monotonic delivery timestamp and sequence, independent of source pacing time.
Consumers can bind the v1.3 tail only after checking its advertised minor version and
table size; older prefixes retain their original layout and behavior.
The QPC mapping and real capture/readback timing still require qualification
on an attached Windows display with WGC access.

The broker supplies
the authenticated immediate caller at bind time, and the provider copies it into
the binding. Frame reads contain only the stream ID and sink, never a caller
string. Unknown and foreign streams both return `MEDIA_CLOSED`.

Raw FP16 CPU frames are bounded to 256 MiB; selected P010 or BGRA delivery
storage is separately bounded to 128 MiB. Readback copies initialized pixels
row by row, including FP16 rows, without reading native padding. Resource frames
are borrowed synchronously; retention returns `MEDIA_UNSUPPORTED`, and a caller
can copy pixels through the public SDK while the callback is active.

Each stream has one joined worker thread. That worker initializes the WinRT MTA,
creates the monitor capture item and a two-buffer `CreateFreeThreaded` frame pool,
and owns the capture session, D3D11 device/context, staging texture, and apartment.
There is no DispatcherQueue dependency. The item-closed handler only sets an
atomic flag and is unregistered at teardown. Session and pool are explicitly
closed before their references and the apartment are released. Temporary COM
references are also released before apartment teardown on failed initialization.

The controller and native binding exchange owned CPU frames through bounded
capacity-one channels. At most one poll is outstanding. Reads use `try_send` and
`try_recv` rather than waiting on the worker; the WinRT queue is polled with
`TryGetNextFrame`, and GPU readback uses `Map(DO_NOT_WAIT)`. Pending staging
textures survive until a later poll. Each captured frame is explicitly closed,
and every successful map is paired with guarded `Unmap`. One compatible staging
texture is reused after its pixels are copied and unmapped. Pending readbacks
cannot be overwritten; mode changes and terminal readback errors discard it.

The worker owns even non-`Send` resources without transferring them across
threads. Closing disconnects the request channel and joins the worker before the
module can unload. Driver or WinRT calls can still stall internally: opening and
joining are not hard real-time operations. The implementation does not abandon a
live worker merely to claim a short shutdown deadline.

The native callback receives at most one tight BGRA8 frame, `stride = width * 4`
and `len = stride * height`. Pixel rows are copied individually; uninitialized
GPU padding is never borrowed or exported. Dimensions are at most 8192 per axis,
and tight pixels are bounded to 128 MiB. Timestamps are monotonic microseconds
from this stream's creation epoch; they are not presentation timestamps.
Sequence increases for each delivered frame. In constant-rate mode, the
controller assigns a fresh timestamp and sequence whenever it repeats cached
pixels.

The frame and its bytes live through the synchronous sink callback. The
consumer must copy or finish processing before return. Constant-rate mode keeps
the last completed frame in owned shared storage; no callback pointer is
retained. No controller mutex is held during the callback, so it may close the
stream without invalidating that frame. A rejected callback consumes the
delivery; its status is returned unchanged. A non-`MEDIA_BUSY` rejection also
clears the cached frame. No frame bytes travel in JSON.

The global Vibeshine `capture = wgcc` configuration route is not implemented
yet. The opt-in open field is the backend behavior needed for that route; its
configuration handoff through NVHTTP, RTSP, and stream-session remains pending.

Native statuses are `MEDIA_PENDING` (no ready CPU frame), `MEDIA_BUSY` (controller
already in use), `MEDIA_CLOSED` (unknown/foreign/ended stream),
`MEDIA_UNSUPPORTED` (unsupported frame format/platform), and
`MEDIA_BACKEND_ERROR` (other failure). A success invokes the sink exactly once.

Geometry is accepted only when GDI, DXGI, and the active CCD topology report the
same complete attached-display set, rectangles, and monitor handles. The query
retains the CCD target device-interface identity so a reused GDI display name
cannot silently select another physical target. Coordinate-producing calls run
under a scoped per-monitor-v2 thread DPI context and restore the previous context
before returning. If the sources are inconsistent or unavailable, WGC can still
capture without `desktop_geometry`; absolute pointer input stays disabled.
Topology can change immediately after validation, so geometry is rechecked at
capture cadence and is not atomic with the frame.

## Verification and remaining gates

Unit tests cover staging reuse and invalidation, caller-owned operations and native
bindings, replay/lookup, callback pixel lifetime, row pitch, cancellation, capacity,
shutdown, incompatible ABI queries, non-`Send` worker ownership, bounded outstanding
polling, joined destruction, and failed initialization cleanup. Windows x86_64 MSVC
code cross-compiles. These checks do **not** qualify real hardware capture.

Physical Windows capture has not been run. Required manual gates include actual
monitor frames, cursor inclusion/exclusion and API availability, capture borders,
rotation and DPI, monitor disconnect/reconnect, mode changes, desktop switching,
locked/secure desktops, multi-GPU copying, sustained memory/latency, and teardown
while the OS delivers closure events. WGC support and monitor interop depend on
the installed Windows API version and session permissions.

HDR preservation/tone mapping, dirty regions, GPU texture sharing, presentation
clock alignment, window capture, and automatic reconnect are not implemented.
The BGRA8 path must not be advertised as preserving HDR. CPU frame bytes are still
allocated per copied frame; the compatible D3D staging texture is reused.
Performance is unqualified.

Behavioral references were the sibling Vibeshine WGC interface declarations in
`src/platform/windows/display.h` and history (`5bb5331a4`, captured adapter identity;
`a37015764`, explicit target retention). No sibling source was copied or translated.
Control/native contracts reuse Amaterasu's existing capture semantics.
Primary references:
[CreateFreeThreaded](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded),
[CreateForMonitor](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createformonitor),
[IsCursorCaptureEnabled](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.iscursorcaptureenabled).
