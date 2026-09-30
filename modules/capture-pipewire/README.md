# Current-user portal/PipeWire capture

This independently built trusted native library provides control and native
`amaterasu.capture-pipewire/1`. It requires Linux with system PipeWire, GIO and
the current user's ScreenCast portal. Build discovery uses pkg-config for
`libpipewire-0.3`, `gio-2.0` and `gio-unix-2.0`; unavailable development libraries
or cross compilation build an explicit unavailable backend. Runtime shared
libraries are distribution prerequisites. Native API grants do not sandbox code.

Settings default to disabled selection and capture:

```json
{"enabled":false,"selection_principals":[],"capture_principals":[],"idle_timeout_seconds":60,"selection_timeout_seconds":120}
```

An administrator-controlled startup policy must enable capture and separately
allowlist authenticated immediate callers in `selection_principals` and
`capture_principals`. The first set may request the portal picker; the second may
enumerate selected metadata and open a capture lease. A picker caller and capture
caller can differ, so runtime call/native grants must independently authorize the
video module that receives frames. Each list permits at most 16 principals. Both
timeouts accept 1 through 300 seconds. The startup availability
probe reads portal version, source and cursor masks only. It never creates a
session or opens a picker, including when disabled. `native_supported` identifies
the built adapter; `availability.available` separately reports the startup probe.
Missing session bus or portal has an explicit reason. No alternate user bus,
headless source, inherited permission, restore token, or persistent grant is used.

All control actions are lower-case:

- `{"action":"status"}` reports `enabled`, `native_supported`, `availability`,
  `selection_state`, `selection`, `selection_choices`, owned `streams`, global
  `frames_copied` and `frames_accepted`, and lease `opened`, `closed`, `active`.
- `{"action":"enumerate"}` requires a capture principal and returns empty
  `outputs` before actual selection.
  `selection_choices` contains `monitor` and/or `window` from actual portal masks.
  The optional offset must be zero and limit is 1 through 256.
- `{"action":"select_source","source_type":"monitor","operation_id":"choose-1"}`
  asynchronously requests a user choice. `window` is also supported. This is the
  only action that can present a portal picker; disabled or unallowlisted calls
  fail before native worker allocation. A successful response initially reports
  `selection_state:"selecting"`. At most one selection or active lease is allowed.
- `{"action":"poll_selection","operation_id":"choose-1"}` returns the
  selecting caller's state, without waiting. States are `selecting`, `ready`,
  `denied`, `failed`; initial global status is `permission_required`. `ready`
  requires the first valid actual CPU frame copied into Rust, not portal node
  metadata alone. On ready, `output` is an object with `id`, actual negotiated
  `width` and `height`, `source_type`, `format:"bgra8"`, `pipewire_node`, and
  `geometry_supported:false`. This opaque output ID is stable for that session.
- `{"action":"cancel_selection","operation_id":"choose-1"}` cancels only
  the selection owner's operation and closes any dependent active lease. Replays
  do not reopen the picker. Use a new operation ID to request a new choice.
- `{"action":"open","output":"portal-...","operation_id":"video-1"}`
  requires a capture principal and acquires its single caller-owned native capture lease. Optional `constant_rate`
  defaults to false; true fails with `unsupported_constant_rate`. Open never
  presents a picker and fails with `source_selection_required` unless ready.
- `{"action":"lookup","operation_id":"video-1"}` recovers only that
  caller's live lease. `{"action":"close","stream":123}` is idempotent for
  its owner and closes the source session. Close atomically stops admission of
  new native frame callbacks. A callback admitted before close can finish after it;
  its immutable frame allocation stays alive until the synchronous callback returns.

Replies use `{"ok":false,"error":{"code":"...","message":"..."}}` for
failure. Selection operation IDs and lease operation IDs are scoped to the
authenticated immediate caller; JSON cannot substitute its owner. Status includes
selection output and operation IDs only for the selection owner or an allowlisted
capture principal; a different capture principal does not receive the owner's
operation ID. Its stream list is owner-filtered, with `frames_accepted` per owned
active stream. Global accepted counters may include a sink that finished
concurrently with a close.

The private C adapter owns GDBus/PipeWire structs and worker cleanup. Selection
uses CreateSession, SelectSources (one monitor/window, hidden cursor, no restore
token) and Start, then opens only the restricted OpenPipeWireRemote FD. Portal
v6 serial targeting is used when supplied; earlier portals target their returned
node while the session is alive. One worker iterates its private GLib context and
PipeWire loop. Bus authentication and method I/O have cancellation/deadlines;
cancel/drop joins the worker after closing request/session, stream, core,
context, FD and loop. No callback can survive provider unload.

The initial CPU path negotiates BGRA/BGRx only. Every dequeued buffer requires
one readable, already mapped MemPtr/MemFd plane, positive adequate stride and
contained chunk offset/size. Before any MemFd pixel callback, `fstat` must prove
the mapped extent fits its backing file. The borrowed FD is never closed;
the trusted provider must retain that extent without concurrent truncation while
the buffer is dequeued. Rust copies initialized pixel rows without padding;
BGRx alpha becomes 255. DMA-BUF-only, unmapped, corrupted, empty and invalid
layouts are rejected; no fake pixels are substituted. Pending rejected frames
remain identical for retry while the bounded latest-frame slot may be replaced.
Native callbacks borrow immutable owned bytes synchronously without controller
or mailbox locks. Resolution/format changes fail the selection and require a
new choice. Portal compositor coordinates never become physical geometry;
CaptureApi v1.1 geometry returns `MEDIA_UNSUPPORTED` and JSON omits
`desktop_geometry`.

There are at most 32 lifetime selection operations and 128 leases per module
instance. Idle selected sources expire without status polling extending them.
Denial, revocation, failure, cancellation, lease close and shutdown release native
resources. Last close/idle expiry/cancellation returns global status to
`permission_required`; owner-scoped polling retains the terminal reason.

Unit tests use a private synthetic driver to verify memory, state and ownership.
They do not qualify actual capture. Actual user permission, nonblack desktop
frames, desktop-to-encoder/public streaming, client playback, high-load behavior
and distribution dependencies remain separate acceptance gates.

Primary references: [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)
and [PipeWire capture interfaces](https://docs.pipewire.org/page_tutorial5.html).
The implementation is original; no Vibeshine capture source was copied.
