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

Current host routing supports 8-bit 4:2:0 SDR with bounded length-prefixed
PyroWave packets, ordinary GameStream packetization, and negotiated FEC. The
encoder separately passes GPU HDR10 tests, but that does not qualify HDR route
selection, metadata delivery, 4:4:4, adaptive FEC, or bandwidth calibration.
Those modes are not advertised. Existing bitrate limits remain in effect.

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
