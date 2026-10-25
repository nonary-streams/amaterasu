# Sustained Moonlight protocol qualification

This test-only wrapper links the developer-supplied Moonlight common C library.
It uses the library's real encrypted RTSP, ENet control, periodic pings, FEC,
peer discovery and video/audio receivers. FFmpeg decodes H.264; libopus decodes
stereo audio and checks that PCM contains nonzero samples. It opens no render,
capture, sound or input device. Only IPv4 loopback RTSP URLs are accepted.

Build and run from the repository root on Linux:

```sh
sh scripts/moonlight_protocol_oracle/build.sh \
  /path/to/moonlight-common-c /tmp/amaterasu-moonlight-oracle
cargo build --workspace --exclude amaterasu-playnite --locked --offline \
  --features amaterasu-encode/software-x264
python3 scripts/test_sustained_moonlight.py /tmp/amaterasu-moonlight-oracle/client
python3 scripts/test_sustained_moonlight.py /tmp/amaterasu-moonlight-oracle/client --public-launch
```

Prerequisites: CMake, a C11 compiler, pkg-config, FFmpeg and Opus development
libraries, OpenSSL and an initialized Moonlight source tree with ENet/nanors.
The build records the supplied source commit and dirty status beside the client;
qualification must record these with its result. Source tested initially:
`3bbe8e818b42a8a1dc3a1381149b7e818fbd33fd` (clean).

The Python runner creates its own signing identity and loopback host. The test
runs two 60-second connections, requires continuous decoded progress in both
lanes, then confirms provider cleanup before reconnecting. `--public-launch`
also pairs an isolated test identity, starts an owned application through pinned
HTTPS with Moonlight's single-controller launch hints, and checks public cancel
reaps the application. Input must remain inactive despite the slot hints. Keys stay in a private
temporary file. A disconnect, stalled lane, decoder error, insufficient decoded
frames or cleanup failure fails the check. `--seconds` can select 1 through 180
seconds for diagnosis; the normal sustained gate is 60.

The fixture's optional `sustain_ms` setting (maximum 300000) selects paced 30 fps
H.264 and 10 ms PCM capture. Default fixture behavior remains eight immediate
H.264-shaped frames. The sustained IDR fixture was generated independently with:

```sh
ffmpeg -f lavfi -i color=c=blue:s=16x16:r=30 -frames:v 1 -c:v libx264 \
  -preset ultrafast -tune zerolatency -profile:v baseline -pix_fmt yuv420p \
  -x264-params keyint=1:repeat-headers=1 -f h264 sustained-idr.h264
```

This establishes protocol/decoder compatibility under the exercised loopback
workload. The default direct-ticket mode omits public NVHTTP pairing/launch;
the public mode checks those separately. Neither establishes real capture,
1080p performance, physical sound playback, LAN loss behavior or Mac GUI playback.
A separate ignored audio test, `sustains_private_monitor_worker_for_one_minute`,
qualifies the native Pulse worker against an owned private null sink; it must run
under the native qualification lock and a fresh before/after host snapshot.

For a separate native desktop qualification, compile `client.c` with explicit
`ORACLE_WIDTH`, `ORACLE_HEIGHT`, `ORACLE_FPS` and `ORACLE_MIN_VIDEO_FPS`
definitions. For example, a 1920 by 1080, 60 fps stream can use a minimum decoded
rate of 45 fps. Record actual decoded counts; passing that threshold does not
establish 60 fps. `ORACLE_REQUIRE_NONZERO_AUDIO=0` permits a quiet native monitor
while still requiring continuous decoded audio packets. Default builds retain
the synthetic fixture's dimensions, progress threshold and nonzero audio check.
