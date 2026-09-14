# Windows capture geometry verification

This internal Rust library verifies desktop geometry shared by the DXGI and WGC
capture providers. It reconciles all attached GDI and DXGI displays with active
CCD source rectangles, and retains the selected CCD target device-interface
identity for later checks. It rejects missing, extra, duplicate, renamed, moved,
or differently sized displays instead of constructing a partial virtual desktop.

Windows coordinate reads and provider selection run under a scoped
per-monitor-v2 thread DPI awareness context. The previous context is restored
before the call returns. A failure to establish or restore that context makes
geometry unavailable. Capture providers may continue without geometry, which
keeps absolute pointer input disabled while preserving relative input and legacy
capture behavior.

The resulting `CaptureGeometry` identifies the selected output rectangle within
the union of the complete physical virtual desktop. Validation is repeated by
the capture provider after open. A topology or retained-target change invalidates
the stream. The checks cannot make geometry and frame capture atomic: Windows may
change topology immediately after the final validation.

Host tests exercise pure reconciliation with synthetic GDI, DXGI, and CCD
snapshots. Windows code is cross-compiled, but no physical desktop is exercised
by these tests.

Primary Windows references:
[QueryDisplayConfig](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-querydisplayconfig),
[EnumDisplayMonitors](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-enumdisplaymonitors),
[DXGI output descriptions](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ns-dxgi-dxgi_output_desc), and
[SetThreadDpiAwarenessContext](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setthreaddpiawarenesscontext).
