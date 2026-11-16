# Optional PyroWave host route

The existing PyroWave encoder can receive capture-owned GPU resources through
`EncoderProvider` and `FrameResourceConsumer`. Video retains capture ownership;
the encoder owns its Vulkan device, pixel conversion, bitstream and framing.
The session coordinator selects the signed route and invokes these interfaces.

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

The local candidate selects 10-bit 4:2:0 PyroWave. A 10-bit wire profile
retains the launch's output range: SDR uses BT.709/sRGB and HDR uses BT.2020/PQ.
Precision and transfer are independent; GPU tests check adjacent ten-bit values
in both ranges through independent decoding. SDR can import both eight-bit and
ten-bit RGB capture; HDR requires capture-owned PQ provenance. HDR metadata uses
the existing reliable control channel and requires the selected video's metadata
interface. Processing and capture provenance checks remain enforced.

Eight-bit encoding remains available in the SDK. The current composition offers
one selected wire profile per launch. 4:4:4, adaptive FEC, bandwidth calibration
and automatic profile fallback are not implemented. Existing bitrate limits
remain in effect. Tests and a signed candidate do not establish client playback.

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
