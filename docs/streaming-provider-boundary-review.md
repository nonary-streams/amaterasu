# Streaming provider boundary review

Codex reviewed the streaming provider changes against Core `c6126205` and
Encode Video `3c45e4bd`. The review follows the client request through the
selected providers and the RTSP and Stream Session lifecycle owners.

## Negotiation and startup

* NVHTTP freezes the encoder selection and supported 10-bit formats for the
  launch. RTSP accepts the exact codec, bit depth, dynamic range and chroma
  tuple before PLAY. Video still requires the selected encoder to prove the
  requested configuration and the capture provider to confirm the source
  mode. Unsupported required media fails through the existing owned launch
  path. An active stream retains its existing selection and media contract.
* An unavailable optional PyroWave runtime leaves the standard encoder
  registered. Video can bind the standard encoder when the missing extension
  has a compatible standard route available. The launch's extension probe
  reports the demotion and leaves the standard offer available before ANNOUNCE.
  A route that proves budget support receives the negotiated wire budget
  through the framer SDK. RTSP keeps the standard codec's software settings
  and projects the extension request through its own configuration contract.
* Video validates the syntax of provider tuning before Open and forwards valid
  settings to the selected encoder for acceptance. A provider refusal is
  associated with the original operation. The owner preserves the requested
  configuration on retry and handles cleanup using the effects that were
  actually acquired.
* Stream Session forwards the exact managed capture binding to the selected
  Video and Capture providers. Consumer-side ownership checks continue to
  bind the capture to its authorized operation and generation. The stock
  PipeWire provider interprets its own KWin selector. An unconfirmed open
  retains its original cleanup ownership.
* The selected audio encoder supplies the standard surround decoder mapping.
  Stream Session validates and freezes the description, and RTSP advertises
  it in DESCRIBE for that ticket. Invalid required audio metadata is a
  preparation failure handled by the existing owner and cleanup path.

## Transport and retirement

* Stream Session checks the ABI shape, declared counts, borrowed buffers, and
  negotiated bounds before forwarding a frame plan. The selected transport
  validates its packet layout, parity and reservation before mutating packet
  state. A busy transport consumes no frame or sequence state. An invalid
  required plan reports the bounded dependency failure on the owning stream
  and follows the same drain and retirement path.
* Live UDP configuration preflight uses the configured ticket capacity and
  asks the selected Media and Control providers to check their respective
  endpoints. A provider uses its own Network binding, closes the temporary
  sockets before acknowledging success and associates its response with the
  original request. A failed or superseded probe leaves the active listener
  configuration in place. A conflicting Open cancels the probe and waits for
  those temporary handles to retire. Disjoint Opens and existing cleanup
  continue. A real session still verifies its allocated ports before SETUP
  advertises them.
* NVHTTP reads the Wake-on-LAN identity for the connection's local endpoint
  through the selected Network provider's optional interface. A missing
  capability or unavailable identity can use the zero MAC response without
  changing stream admission.

See also the [lifecycle review](stream-lifecycle-behavior.md),
[wire negotiation contract](../crates/gamestream/src/rtsp/negotiation/README.md)
and [Network SDK contract](network-plugin-sdk.md).

## Verification

The Linux suites for NVHTTP, RTSP, Stream Session, Video, Encode, PipeWire,
Launch, and the lifecycle coordinator passed. The final transport run also
passed Media Transport, Control Transport, and Module SDK. Replacement tests
exercise a non-stock frame plan and a busy retry, UDP preemption with an
outstanding provider bind, an exact capture binding through retirement,
Surround DESCRIBE, and encoder settings through standard startup. The GPU
feature test loads the pinned PyroWave library with no available Vulkan device
and checks that standard H.264 can still open and retire.

Strict Clippy, Rust formatting, C ABI layout checks, and Git diff checks passed
for the affected paths. These checks cover source and local fixtures. Native
Windows execution, installed-package activation, and live client playback
remain unverified.
