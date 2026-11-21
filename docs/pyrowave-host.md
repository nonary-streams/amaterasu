# Optional PyroWave host route

The existing PyroWave encoder can receive capture-owned GPU resources through
`EncoderProvider` and `FrameResourceConsumer`. Video retains capture ownership;
the encoder owns its Vulkan device, pixel conversion, bitstream and framing.
The session coordinator selects the signed route and invokes these interfaces.
The SDK's `PresentedDmaBufResource` adapts the already captured resource at the
encoder boundary, using the video timeline while retaining capture ownership,
source pacing, color and synchronization. Capture control and the selected
capture provider stay unchanged.

Build `amaterasu-encode` with `pyrowave-embedded,linux-nvenc-cuda` and
`amaterasu-video` with `submitted-extensions`. Set
`AMATERASU_PYROWAVE_EMBED_LIBRARY` to the pinned library built by
`scripts/build_pyrowave.py`. The embedded dependency is loaded from a sealed
memory file and is covered by the encoder component's signed digest. The video
worker keeps synchronous GPU work off the shared host tick.

The optional route uses selector 3, payload marker 99/PYROWAVE, and bitstream
identity `186f0393`. It requires correlated source, framing and transport probes
before RTSP offers the route. Startup waits for a complete encoded keyframe
before acknowledging PLAY. Transport validates the complete frame plan before
admitting UDP bytes; normal audio, input and lifecycle services remain selected.
The selected encoder's control service must be granted through the complete
NVHTTP, RTSP, stream-session and video delegation chain, in both the signed
manifest and startup policy. The broker intersects ancestor authority, so a
direct video probe alone does not qualify a normal launch.
The signed profile in `scripts/pyrowave/profile.json` also declares the client's
record-framing announcement and disabled adaptive modes. The optional peer link
hint caps the configured wire rate at 80% of the reported link. Pacing hints are
accepted without enabling bandwidth calibration. Compression and
undeclared critical attributes remain rejected. Optional attribute declarations
extend the JSON profile contract; they do not change the native ABI. Rebuild
encode, video, media-transport, stream-session and RTSP together when using them.

For PyroWave, Moonlight's configured bitrate is the total wire budget. Moonlight
caps the legacy GameStream encoder field at 100 Mbps. RTSP applies the host
bitrate cap and passes the configured budget to stream-session, which reserves
audio and network overhead using the prepared transport limits. This route uses
length-prefixed frames with full-frame protection and permits up to four FEC
blocks per frame. A client that omits the configured bitrate uses the bounded
legacy field.

The local candidate selects 10-bit 4:2:0 PyroWave. A 10-bit wire profile
retains the launch's output range: SDR uses BT.709/sRGB and HDR uses BT.2020/PQ.
Precision and transfer are independent; GPU tests check adjacent ten-bit values
in both ranges through independent decoding. SDR can import both eight-bit and
ten-bit RGB capture; HDR requires capture-owned PQ provenance. HDR metadata uses
the existing reliable control channel and requires the selected video's metadata
interface. Processing and capture provenance checks remain enforced.

The host offers one PyroWave profile per launch. When its optional probe
fails, the host records a warning and offers the standard codecs supported for
that launch. Moonlight must honor the launch's DESCRIBE to choose a standard
codec before ANNOUNCE. When the client asks for HDR, the fallback needs a
compatible 10-bit codec. Once both sides agree on a format, startup requires
a keyframe in that format.

PyroWave applies the color range from the client's ANNOUNCE during pixel
conversion. SDR uses the negotiated BT.601 or BT.709 matrix, and HDR uses
BT.2020/PQ at either range. The sequence header carries the actual range.
BT.601 is signaled by GameStream's color negotiation because the PyroWave
sequence header has no BT.601 matrix identifier.

The VKMS snapshotter tries renderable scanout layouts if it cannot allocate a
renderable linear buffer. This preserves RGB precision and provides a shared
DMA-BUF layout on GPUs whose render-only buffers cannot be imported by
Vulkan. SDR PyroWave can use either 8-bit or 10-bit RGB capture while keeping
the selected wire precision. 4:4:4, adaptive FEC and bandwidth calibration
remain unimplemented.

For a compatible installed source set, `scripts/prepare_pyrowave_update.py ROOT
OUTPUT` prepares a signed candidate and exact proposed policy without changing
the installation. It retains the bytes of every unaffected component. Review
`metadata.json`, `policy-delta.json`, and the native manifest. Validate against
the installed host and runtime before importing; source provenance alone does
not establish compatibility. Use the normal signed local-import and activation
procedure in [local-package-updates.md](local-package-updates.md). Check stream
activity immediately before activation; interrupting a live stream requires
fresh permission. Report GPU tests, package activation and real client playback
separately.
`cargo run -p amaterasu-package --example check-policy-upgrade -- ROOT OUTPUT`
checks the prepared policy with the real activation guard without changing the
installation, including a later update that retains the approved destination.

The protocol regression fixture `moonlight-pyrowave10.sdp` is generated by
Moonlight's production `SdpGenerator.c`, not by replacing H.264's selector in a
fixture. Its generator SHA-256 is
`cc66907a4033768c307c3a0de87ae90da34c7c2a10a32599168f4a6ddd3bfdf4`.
To reproduce it, compile `scripts/moonlight_protocol_oracle/pyrowave_sdp.c`
with that source and its `src` and `enet/include` include directories. The helper
opens no connection. Tests drive its output through the complete RTSP negotiation
and verify that SDR/HDR starts use the same configuration as source preflight.
This qualifies the protocol path; GPU round trips and real client playback are
separate checks.
