# Synthetic stream-session source

This test-only module replaces the production `amaterasu.video/1` and
`amaterasu.audio/1` providers in the signed StreamProbe composition. It never
captures a desktop, opens an audio device, or produces decodable H.264. It exists
to exercise the real session orchestrator, Opus encoder, encrypted transports,
native ownership, and cleanup. Do not install it in a production composition.

Package/component: `amaterasu-stream-source` / `amaterasu.stream-source`.
Library: `amaterasu_stream_source`. No settings are required. Both replacement
services provide their normal control and native interfaces. `test.stream-source/1`
is a control-only metrics service; `{"action":"status"}` returns:

```json
{
  "ok": true,
  "fixture": true,
  "video": {"opened": 0, "closed": 0, "active": 0, "frames": 0, "fragments": 0, "force_idr": 0},
  "audio": {"opened": 0, "closed": 0, "active": 0, "frames": 0}
}
```

Video open requires `output:"synthetic-display"` and `packet_boundaries:true`.
The pipeline's capture-provider, encoder and configuration fields are accepted as
fixture labels, without selecting hardware. Open/status/lookup return `state:
"running"` (or `"draining"`) and confirm packet boundaries. `force_idr` counts the
request; every fixture access unit already carries a keyframe flag. `drain` emits
all remaining fixture output, then exactly one EOS. Exhausted streams return
PENDING until drain. Close is synchronous and status/lookup then return
`stream_not_found`.

Each video stream emits eight 1,700-byte units with sequence `s=0..7` and timestamp
`s*10000` microseconds. Bytes 0 through 4 are `00 00 00 01 65`; every byte at index
`i=5..1699` is `((i*17+s*13)%251)+1`. Each unit is delivered as two 850-byte fragments
with identical sequence/timestamp. The first has KEYFRAME; the second has
END_OF_ACCESS_UNIT. These bytes test framing and parity, not H.264 decoding.

Audio open accepts `default` or `synthetic-render`, resolving both to the concrete
`synthetic-render` endpoint. It emits eight 480-frame packets of 48 kHz stereo i16
PCM with channel mask 3. For packet sequence `s` and sample frame `f`, the left
sample is `((f+s*13)%100-50)*100` and right is its negative, serialized little
endian. Timestamps are `s*10000` microseconds. `synthetic-unsupported-format`
explicitly opens a 44.1 kHz endpoint with 441 frames per packet, for post-open
format rejection and cleanup testing. Exhaustion returns PENDING; audio capture
has no encoded EOS contract.

Operation IDs are optional, bounded, and scoped to immediate caller and service.
Identical live opens return the same stream; conflicting requests fail. Historical
IDs never silently reopen a closed stream. Native bindings copy the runtime caller
and cannot read another caller's stream. Rejected sinks retain identical output
for retry. There are at most 16 live streams and 128 total allocations per module
lifetime. Lost completion of a new open closes that allocation; replay completion
failure leaves an existing allocation alone.

Unit tests cover operation ownership, finite output, fragment metadata, exact
retry bytes, EOS, unsupported format metadata, native caller lifetime and busy
reentry behavior. Hardware and decoder qualification are deliberately outside
this fixture.

Startup `{"desktop_geometry":true}` enables a fixed physical geometry snapshot
for video Open/Lookup/Status: output `(-1920,0,1920,1080)` within virtual desktop
`(-1920,-200,3840,1440)`, expressed as left/top/width/height. It is omitted by
default. These coordinates deliberately differ from the encoded 16×16 mode.
The test-only metrics action `invalidate_geometry` then makes native video reads
fail, exercising session failure and held-input cleanup. This simulates the
production geometry watchdog; it does not enumerate or change desktop monitors.
