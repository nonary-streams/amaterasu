# Linux session profile

`PackageProfile::LinuxSession` composes a host a Moonlight client can reach on
Linux. It is built with `amaterasu-release init-linux-session` and
`amaterasu-release pack-linux-session`, and it is separate from two profiles it
must not be confused with:

| Profile | Purpose |
| --- | --- |
| `LinuxProbe` (`pack-linux-probe`) | Qualification fixtures: portal and X11 capture, test-only audio and input. |
| `Platform` (`pack`/`pack-platform`) | The Windows product composition. |
| `LinuxSession` (`pack-linux-session`) | Real input, audio, KMS capture, virtual display and encoder libraries. |

The policy that grants each module its services lives in the root's
`bootstrap.json`, which `init` writes. That is why the Linux session profile has
its own `init-linux-session`: a root initialised with plain `init` carries the
development policy and cannot stage a Linux session package.

Nothing here is a claim that a real client has streamed. See
[Gaps and pending checks](#gaps-and-pending-checks).

## Safe defaults

* Every effect-bearing component starts disabled: `amaterasu.linux-session-video`
  and `amaterasu.capture-kms`, `amaterasu.audio`, `amaterasu.virtual-display`.
  `amaterasu.input` has `allow_injection=false` (and the stream session has
  `enable_input=false`), and `amaterasu.display` has `allow_mutation=false`.
  Nothing touches the running KDE session, `Virtual-1`, `HDMI-A-1` or another
  host until an operator passes the option that enables it.
* Every listener binds loopback. Moonlight's ports use the conventional base
  47989 on `127.0.0.1` unless `--lan` names another port (see below).
* The management API is off. `--management-port` enables it on `127.0.0.1` only.
* The encoder (`amaterasu.encode`) and the Opus encoder are on. They only
  compute: they open no display, sound or input device. The codec capability a
  client reads before it can start a stream comes from the encoder.
* Persistent state lives in `ROOT/data/state` (and `session-history` below it),
  directories created with mode 0700. `ROOT` itself is also private.

The updater, system tray, UPnP, host configuration, diagnostics and Playnite are
not in the package at all. Leaving a component out is the only dependable way to
keep it disabled: the updater has no bootstrap-level switch and would otherwise
look for a release on start. The effect is that stream-start notifications to the
updater and tray are refused by the broker, which the stream session treats as
"nothing to notify", and pairing PIN prompts are available from the console or
the management API rather than a tray notification.

## Build recipe

Build the host, the publisher and one library per component. Use two jobs and a
low priority on a machine that is also running a desktop and games.

```sh
export CARGO_TARGET_DIR=$HOME/sources/amaterasu-targets/linux-session
env CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 nice -n 10 \
  cargo build --offline --locked \
    --features amaterasu-encode/software-x264 \
    -p amaterasu -p amaterasu-release \
    -p amaterasu-runtime -p amaterasu-state -p amaterasu-session \
    -p amaterasu-configuration -p amaterasu-session-history -p amaterasu-host-metrics \
    -p amaterasu-catalog -p amaterasu-application -p amaterasu-steam -p amaterasu-lutris \
    -p amaterasu-authentication -p amaterasu-management -p amaterasu-pairing \
    -p amaterasu-nvhttp -p amaterasu-display -p amaterasu-launch -p amaterasu-input \
    -p amaterasu-virtual-display -p amaterasu-capture-kms -p amaterasu-audio \
    -p amaterasu-audio-encode -p amaterasu-encode -p amaterasu-video \
    -p amaterasu-media-transport -p amaterasu-control-transport \
    -p amaterasu-frame-limiter -p amaterasu-stream-session -p amaterasu-rtsp
```

`scripts/test_linux_session_profile.py` runs this same build.

* **Software x264.** `--features amaterasu-encode/software-x264` compiles the
  binding. It needs `x264.h` (found through `pkg-config x264` or
  `AMATERASU_X264_INCLUDE_DIR`) and `libx264` at run time. Without the headers
  the build still succeeds but warns `x264 unavailable`, and the encoder reports
  `software_encoder.available=false` with a reason. x264 is GPL-2.0-or-later (or
  commercial), so shipping it is an explicit choice.
* **NVENC.** There is no cargo feature. Once the Linux NVENC backend lands in
  the encoder, it compiles in automatically when the ffnvcodec and CUDA headers
  are present. It is not in `main` today, so the profile currently encodes with
  software x264 only.

## Initialise, pack, stage, run

```sh
amaterasu-release init-linux-session ROOT [options]
amaterasu-release pack-linux-session ROOT ARTIFACTS PACKAGE 1
amaterasu-release verify ROOT PACKAGE
amaterasu-release stage ROOT PACKAGE
amaterasu serve ROOT          # development console on standard input
```

`ARTIFACTS` is `$CARGO_TARGET_DIR/debug` (or `release`). `init-linux-session`
refuses a root that already exists as a trust identity; policy options are fixed
when it runs. To change an option, initialise a new root (pairing identity is
imported again with the console `import_identity` call, or created by `initialize`).

## Options

| Option | Effect |
| --- | --- |
| `--lan ADDRESS:PORT` | Base port and a concrete bind address for NVHTTP, RTSP, media and control. See [LAN option](#lan-option). |
| `--allow-remote` | With a wildcard `--lan` address, explicitly requests listening on every interface. |
| `--catalog-owner management\|console` | Whose application list Moonlight reads. Default `management` (entries authored through the management API); `console` is for development roots that author entries on the console. |
| `--management-port PORT` | Enables the management API on `127.0.0.1:PORT` (1024 or higher). Needs a staged web bundle (`pack-web`/`stage-web`). |
| `--capture-output ID` | The KMS output Moonlight streams. Default `amaterasu-managed-output`, which deliberately matches no physical output. |
| `--audio-endpoint NAME` | Audio endpoint selector passed to the audio module. Default `default`. |
| `--enable-capture` | Enables the video module and KMS capture (`capture_providers=["kms"]`). |
| `--enable-audio` | Enables host audio capture. |
| `--enable-virtual-display` | Enables the owned virtual display provider. |
| `--enable-input` | Sets `amaterasu.input` `allow_injection=true` and stream-session `enable_input=true`. A relative pointer can leave an owned screen and move the live cursor; keep this off on a shared desktop. |
| `--enable-display-mutation` | Sets `amaterasu.display` `allow_mutation=true`. |

Each option may appear once. Unknown options, a missing value, or an out-of-range
value stops `init-linux-session` before it creates anything.

## LAN option

By default the host listens only on loopback, so another device cannot reach it.
`--lan ADDRESS:PORT` sets the base port and the address for NVHTTP, RTSP, the media
transport and the control transport together:

```sh
init-linux-session ROOT --lan 192.168.1.20:47989          # one concrete interface
init-linux-session ROOT --lan 0.0.0.0:47989 --allow-remote # every IPv4 interface
init-linux-session ROOT --lan '[::]:47989' --allow-remote  # IPv4 and IPv6
```

Rules, matching NVHTTP's and RTSP's own network settings:

* The base port is 1029 through 65514. NVHTTP's HTTPS listener is `base - 5`,
  RTSP is `base + 21`, and the video, control and audio UDP ports are `base + 9`,
  `base + 10` and `base + 11`. With the default base 47989 that is 47984, 48010
  and 47998 through 48000.
* A concrete address (for example the host's LAN address) enables remote binding
  only on that one address. A concrete loopback address only moves the ports.
* A wildcard (`0.0.0.0`, `::`) is rejected unless `--allow-remote` is also given.
  `--allow-remote` alone, or with a concrete address, is rejected.
* Multicast, broadcast and IPv4-mapped IPv6 addresses are rejected.
* `allow_remote`, `address_family` and `bind_address` are written identically to
  NVHTTP, RTSP, the media transport, the control transport and the stream session.

The option never changes a firewall, router or UPnP mapping. If the host's
firewall blocks the ports, the operator opens them; this profile does not.
The standard Moonlight port 47989 may already be taken on a machine that also
runs another game-streaming host, so pick another base port with `--lan`.

## Grant chain

A request made on behalf of another carries the intersection of every ancestor's
call grants, so each service a stream reaches appears on every component above it.
The policy grants exactly this chain, and the tests pin it:

* `nvhttp -> rtsp -> stream-session -> video / media-transport / control-transport`
* `video -> encode / capture-kms`
* `stream-session -> session / display / virtual-display (create, status, remove) / input / audio / audio-encode`
* `nvhttp -> launch / catalog / application / session / input / pairing / encode`
  (the `encode` grant feeds the capability snapshot in `/serverinfo`)

`RTSP` accepts only `nvhttp`, the stream session accepts only `rtsp`, and the media
and control transports accept only the stream session. Only the stream chain,
the video module and read-only management enumeration can call `capture-kms`;
only NVHTTP, RTSP and the stream session can call `virtual-display`.

Authentication has its own authorization generation (5). Advance it with any
change to the administrator principals or allowed scopes, including a revert;
TTL edits do not need it.

Capture is `amaterasu.capture-kms` through the video component
`amaterasu.linux-session-video`. The one capture library serves both physical KMS
outputs and managed virtual outputs.

## First stream

The first stream is capped at 1920x1080 at 60 Hz. Software x264 cannot sustain
the host desktop's own 3024x1890 at 90 Hz; larger or faster requests are refused
rather than degraded silently. Streams are SDR H.264 only.

## Verification

```sh
cargo test -p amaterasu-release
cargo fmt --all --check
cargo clippy -p amaterasu-release --all-targets -- -D warnings
python3 scripts/test_linux_session_profile.py
```

The release tests check unique providers and dependencies, that no probe, DXGI,
WGC, PipeWire or X11 component is present, the delegated chain (including through
a real broker), disabled effect defaults, loopback defaults, LAN option
validation against NVHTTP's own settings parser, and the authorization-generation
pin. The script needs the Python `cryptography` package. It packs and verifies the
profile, stages it into a private temporary root, runs the host with an isolated
home directory and no session bus, display or sound-server variables, pairs a wire
client over loopback, lists a console-authored application, checks that every
socket the host owns is loopback, restarts, and confirms `PairStatus=1`. It also
asserts that wildcard, out-of-range and conflicting `--lan` values are refused.

Binding beyond loopback is a separate, ignored case. Run it only where exposing the
ports is intended: `python3 scripts/test_linux_session_profile.py --ignored`.

## Gaps and pending checks

Implemented and tested here: the profile, its policy, the options, and loopback
pairing/application-list/restart behaviour.

Not implemented, and not claimed:

* **No mDNS/DNS-SD announcement.** Clients must add the host by IP address.
* **No `/resume`.** A client that disconnects must start a new launch.
* **SDR only.** HDR and 10-bit paths are not part of this profile.
* **Software x264 until NVENC lands**, hence the 1920x1080 at 60 Hz cap.
* **Settings added after their owners land.** The session `journal_directory`
  (session ledger) and catalog `seed_desktop_entry` (desktop launch kind) are
  not set here yet: module settings parse strictly, so naming them early would
  stop those modules starting.
* **Playnite, updater, tray, UPnP, diagnostics and host configuration** are absent
  by design (above).

Pending runtime checks (nothing here has run them): reachability from another
device on the LAN; real Moonlight pairing and streaming
(`qualification-real-client-harness`); mDNS discovery; and any capture, display,
audio or input effect, which the owning tasks qualify under the native lock with
a fresh baseline.
