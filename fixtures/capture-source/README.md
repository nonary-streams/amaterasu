# Test-only NV12 capture source

`amaterasu-capture-source` is a trusted native test fixture. Load it only in a
test profile under a separate module identity, providing control and native
`amaterasu.capture-dxgi/1` plus control `test.capture-source/1`, with empty settings
`{}`. Do not also load the genuine DXGI provider for the same service. With
`{"custom_service":true}` it instead registers control and native
`test.custom-capture/1`, allowing integration tests to exercise an independently
named capture service without registering the DXGI identity.

The sole output, `nv12-fixture-display`, is 64×48 with matching output and virtual
desktop geometry at (0, 0). Each stream produces exactly 12 accepted frames,
with 30 fps timestamps. Frames use NV12 with stride 64: 3,072 initialized Y bytes
followed by 1,536 interleaved UV bytes at neutral chroma 128. The left and right
vertical halves alternate luma 220 and 32 on every accepted frame. This provides
a visible, compressible moving source for the real software encoder.

The v1.1 capture interface borrows immutable frame bytes only during its
synchronous sink callback. It never waits for a frame or invokes a sink while
holding the controller lock. A busy or rejected sink accepts nothing; retry
receives the same sequence and pixels. After 12 accepted frames, reads return
`MEDIA_PENDING` until the caller closes the stream. The video pipeline owns
wall-clock pacing and encoder drain; this fixture does not emit encoded EOS.

Control supports `status`, `enumerate`, `open` with `output` and optional
`operation_id`, `lookup` by operation ID, and `close` by stream ID. Stream and
operation ownership uses the authenticated immediate runtime caller, including
the real video's downstream caller identity, rather than an identity supplied
in JSON. Four active streams and 128 total opens bound all retained state.
Open retries with an operation ID return the same live stream. Failed request
completion rolls back only a newly created stream. Exact-ID closes are
idempotent for their owner; closed IDs cannot be read or looked up.

`test.capture-source/1` accepts `{"action":"status"}` and reports global
`opened`, `closed`, `frames`, and `active`, plus the same counters by immediate
caller in `callers`. `frames` counts accepted sink callbacks. Fixture metadata
explicitly identifies this as test-only and leaves native capture unqualified.

This fixture exercises native ABI delivery, real video routing and real encoder
behavior. It makes no claim about desktop capture, GPU adapters, hardware
encoders, display handling, or supported-platform capture parity. API grants
do not sandbox trusted native code.
