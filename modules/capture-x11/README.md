# X11 capture

This native provider enumerates currently attached RandR outputs and reads the
selected output rectangle from the X11 root window with `xcb_get_image`. The
adapter validates the selected output and mode again before each read, converts
the server's TrueColor visual masks and byte order into owned BGRA pixels, and
rejects unsupported visuals, rotations, mode changes, and frames over the media
ABI size limit. Each open stream owns one XCB connection and closes it when its
caller closes the stream or the module shuts down.

The provider uses only the explicit `DISPLAY` inherited by the host process. It
does not guess `:0` when that variable is absent, open a portal, create a display,
or synthesize test pixels. The provider is disabled by default and must also be
listed in the signed video component's `capture_providers` policy. Access to an
X server is governed by the user's Xauthority/session permissions; module call
grants are not an operating-system sandbox.

X11 readback copies pixels through the X server and is a compatibility capture
path, not a zero-copy path. Cursor composition is not currently provided.
Environments with no reachable X server, an unsupported visual/rotation, or an
output mode that changes while streaming report an explicit unavailable or
access-lost result. The provider has no fallback to PipeWire or another display
server. Real-window pixel and throughput qualification requires a live X11 or
Xwayland session with an authorized `DISPLAY` and remains a separate native
platform gate.
