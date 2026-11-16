# Best-effort stream compatibility

A usable stream should survive optional client hints and unavailable optional
features. The protocol adapter records each adjustment with the requested and
effective values. The host log exposes these adjustments in Diagnostic Alerts
without requiring Stats. When session history is available, the stream owner
also records the adjustments against that exact stream; unavailable history
never blocks startup.

Current behavior:

- Stale refresh-rate hints are ignored in favor of requested FPS.
- Omitted legacy codec, color, range, audio duration and control hints use
  compatibility defaults. Actual codec, encryption and provider checks remain.
- Initial/minimum bitrate hints need not equal maximum bitrate. A nonzero
  configured bandwidth budget reserves video FEC, audio and packet overhead.
- Unsupported high-quality audio and intra refresh use configured/default
  behavior and produce an alert instead of rejecting the stream.
- The generic media path admits dimensions through 8192 per axis and frame
  rates through 1000 FPS, within signed profile limits and provider checks.
  These are validation ceilings, not claims that every encoder supports them.
- Ended shared transports do not consume active-client slots; their cleanup
  records remain retained.
- Existing capture/encoder initialization fallbacks report their failure reason
  and attach it to the stream's history. Runtime fallback evidence is separate
  from the immutable start request, so retries retain their original identity.

A fallback must preserve the client's negotiated wire format. A host cannot
silently substitute H.264 for HEVC, or 4:2:0 for requested 4:4:4.
The NVENC/CUDA provider supports 4:4:4 with driver-verified capability flags,
full-resolution chroma conversion, and matching H.264/HEVC profiles. Ten-bit
SDR/HDR uses planar 16-bit 4:4:4 input. Other providers must explicitly support
the format; they cannot substitute 4:2:0. Security,
resource ownership and uncertain-effect retirement checks are not optional
feature failures.

Alerts currently use bounded host log retention and refresh with that log.
History retention follows the enabled Stats/history provider's policy.

## 4:4:4 validation

The shared format includes chroma sampling; absent sampling remains 4:2:0 and
legacy JSON is unchanged. Codec advertisements use explicit per-provider 8/10-bit
4:4:4 capabilities. NVENC rechecks device capabilities at open. Full-resolution
planar chroma is produced on the GPU for BGRA, DMA-BUF and 10-bit CUDA inputs.
The encoder probe allocates the selected sampling layout. A provider returning
4:2:0 for a 4:4:4 probe is rejected before capture opens.

Local NVIDIA hardware qualification verifies H.264/HEVC 8-bit 4:4:4 bitstreams
with FFprobe, preserves alternating single-pixel chroma edges, and verifies
10-bit SDR/HDR 4:4:4 with identical output through DMA-BUF and CUDA input.
AV1 4:4:4 is offered only when the device reports support; this hardware did not
qualify it. These are isolated encoder tests, not an installed Moonlight session.
