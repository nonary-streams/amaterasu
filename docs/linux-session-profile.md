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
export CARGO_TARGET_DIR=/run/media/chasep/ArchivedGames/amaterasu-targets/linux-session
git submodule update --init third-party/libvirtualdisplay
env CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 nice -n 15 \
  cargo build --offline --locked \
    --features amaterasu-encode/software-x264,amaterasu-encode/linux-nvenc-cuda,amaterasu-virtual-display/linux-libvirtualdisplay \
    -p amaterasu -p amaterasu-release \
    -p amaterasu-runtime -p amaterasu-state -p amaterasu-session \
    -p amaterasu-configuration -p amaterasu-session-history -p amaterasu-host-metrics \
    -p amaterasu-catalog -p amaterasu-application -p amaterasu-steam -p amaterasu-lutris \
    -p amaterasu-authentication -p amaterasu-management -p amaterasu-pairing \
    -p amaterasu-nvhttp -p amaterasu-display -p amaterasu-launch -p amaterasu-input \
    -p amaterasu-virtual-display -p amaterasu-capture-kms -p amaterasu-capture-pipewire \
    -p amaterasu-audio \
    -p amaterasu-audio-encode -p amaterasu-encode -p amaterasu-video \
    -p amaterasu-media-transport -p amaterasu-control-transport \
    -p amaterasu-frame-limiter -p amaterasu-stream-session -p amaterasu-rtsp
git submodule update --init modules/capture-vkms
env CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 nice -n 15 \
  cargo build --offline --locked --manifest-path modules/capture-vkms/capture/Cargo.toml
```

The provider uses its own lockfile and pinned SDK. Fetch its dependencies before
using `--offline`. Both builds share the `CARGO_TARGET_DIR` above.

`scripts/test_linux_session_profile.py` uses software encoding and loopback fixtures.
It does not verify the native GPU encoder or the Linux virtual-display backend.

* **Software x264.** `--features amaterasu-encode/software-x264` compiles the
  binding. It needs `x264.h` (found through `pkg-config x264` or
  `AMATERASU_X264_INCLUDE_DIR`) and `libx264` at run time. Without the headers
  the build still succeeds but warns `x264 unavailable`, and the encoder reports
  `software_encoder.available=false` with a reason. x264 is GPL-2.0-or-later (or
  commercial), so shipping it is an explicit choice.
* **NVENC.** `amaterasu-encode/linux-nvenc-cuda` explicitly enables the native
  NVIDIA backend. It needs nv-codec-headers and EGL/GL/GBM development headers.
  Verify the running encoder enumeration includes the hardware codecs; a
  successful software-only build does not establish native encoder support.
* **Linux virtual displays.** `amaterasu-virtual-display/linux-libvirtualdisplay`
  explicitly enables the Linux backend. Without it the plugin uses the
  unsupported-platform path on Linux, and owned-display streams fail before
  sending media. It builds the pinned userspace driver library, not a kernel
  module, and does not reload the installed display driver.

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
| `--enable-capture` | Enables the video module and KMS capture. |
| `--enable-audio` | Enables host audio capture. |
| `--enable-virtual-display` | Enables the owned virtual display provider and defaults its capture to the VKMS plugin when no physical output is specified. Capture still requires a configured and authorized helper. |
| `--enable-virtual-screen-capture` | Selects the existing `virtual_screen` alias for the VKMS plugin and requires `--enable-capture`, `--enable-virtual-display`, the default capture output, and one helper mode below. |
| `--capture-authority-helper ABSOLUTE_SOURCE` | Packages helper bytes and requests broker-mediated authority for the capture plugin. Requires `--enable-capture`, `--enable-virtual-display`, and the default capture output. This declares a request, not an approval or installation. |
| `--capture-authority-card CARD` | Exact DRM card scope for the packaged request, from 0 through 63; default 0. Requires `--capture-authority-helper`, including when CARD is explicitly 0. |
| `--frame-helper ABSOLUTE_PATH` | Legacy direct-helper mode, mutually exclusive with `--capture-authority-helper`. The root-owned capability-bearing executable must already be installed and authorized separately; it is independent of package approval. |
| `--enable-input` | Sets `amaterasu.input` `allow_injection=true` and stream-session `enable_input=true`. A relative pointer can leave an owned screen and move the live cursor; keep this off on a shared desktop. |
| `--enable-display-mutation` | Sets `amaterasu.display` `allow_mutation=true`. |

Each option may appear once. Unknown options, a missing value, or an out-of-range
value stops `init-linux-session` before it creates anything.

### Package-approved virtual-screen capture

For the package authority mode, also build `amaterasu-capture-vkms` with
`linux-kwin-geometry`, plus `amaterasu-vkms-frame-helper` and
`amaterasu-capture-authority`. Build the helper separately from
`modules/capture-vkms/helper-wire/Cargo.toml`. The optional `linux-gpu-snapshot` feature needs the EGL/GBM build
prerequisites and a usable GPU render node at runtime. The normal profile build
must include every component in its composition; the helper and broker are
separate binaries.

`init-linux-session` records the local helper source in
`capture-authority-package.json`. `pack-linux-session` copies those bytes into
the signed package and records the exact helper digest, component and card
scope. The new mode sets `authority_channel:true` and `gpu_snapshot:false`; a
missing approved channel is an explicit availability
failure and does not fall back to the legacy helper.

Run `capture-authority-review` to inspect the verified publisher, package,
component, helper, worker and scope. A later explicit administrator invocation of
`capture-authority-approve` checks the confirmed approval ID, installs a protected
snapshot and records the decision. It seals the copied bootstrap as a signed
production policy without modifying the local publisher root. The protected
broker then starts a new unprivileged worker with a private inherited authority
channel. It requires Yama `ptrace_scope` 1, 2 or 3; at scope 1, launch it through
the root system service manager rather than beneath a user shell. For KWin
geometry, explicitly configure the approved user's `WAYLAND_DISPLAY` socket
basename in that root service; the broker validates its owner and forwards it
without inheriting arbitrary environment values. The workflow does not change
kernel settings. See the [complete approval and revocation commands](capture-plugin-privileged-authority.md#prepare-review-and-deploy-for-a-later-test).
No profile-generation command installs a privileged helper, starts a service or
proves a real frame export.


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

Authentication has its own authorization generation (9). Advance it with any
change to the administrator principals or allowed scopes, including a revert;
TTL edits do not need it.

The video component `amaterasu.linux-session-video` uses `amaterasu.capture-kms`
for an explicitly named physical output. With an owned virtual display and the
default output, it selects the independently built `amaterasu.capture-vkms`
plugin. Without a configured helper that plugin remains disabled and the
virtual capture is unavailable; it does not silently fall back to PipeWire.
Without an owned display, the default is the user-selected PipeWire source.
The default provider identity is `vkms`; profiles explicitly configured with
`--enable-virtual-screen-capture` retain their existing `virtual_screen` alias.

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
* **Native colour modes require native qualification.** HEVC/AV1 support
  ten-bit SDR and guarded HDR10 with the NVIDIA backend. See
  [HDR capture](hdr-capture.md); fixture/profile tests do not prove HDR capture
  or client playback.
* **Software x264 remains capped at 1920x1080 at 60 Hz.** Hardware limits are
  reported by the selected encoder.
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

## Browser HTTPS on a private network

For an initialized development Linux session root with its component and web packages staged,
select a concrete RFC 1918, 100.64.0.0/10 VPN, or IPv6 ULA address assigned to this host. Stop the Amaterasu host
before provisioning; the setup command takes the same root lock as the host and
fails if it is busy. This does not change the Moonlight listeners or firewall.

```sh
python3 scripts/setup_linux_management.py /absolute/path/to/ROOT --address 192.168.1.20
```

The default HTTPS port is 48081; `--port` selects another. The script retains the
loopback HTTP setting, creates a separate private management certificate and key
with the selected IP in its subject alternative names, and displays the URL and
SHA-256 fingerprint. Repeat the command with the same address to reuse the
identity. To use a certificate trusted by browsers, pass `--certificate` and
`--private-key` with matching PEM files; the key must be owned by the current
user and inaccessible to other users. The certificate must currently be valid, permit TLS server authentication, and cover the selected IP.
Changing the address requires a suitable replacement identity. Existing roots keep their saved authentication policy; setup warns if it predates the account scopes and restricted management verifier. Upgrade that policy separately before using accounts, preserving its other settings and advancing its authorization generation. The generated
self-signed certificate is not automatically trusted: install or approve its
certificate in the browser using the browser's trust procedure, after checking
its fingerprint through a trusted local channel. Keep the private key private.

For a fresh root, issue the short-lived, single-use enrollment code locally while
the host is stopped, after staging the active components:

```sh
python3 scripts/enroll_linux_management.py /absolute/path/to/ROOT /absolute/path/to/amaterasu
```

This runs a temporary local worker under the root lock and exits before the
normal host starts. It displays the URL, fingerprint, code and expiry; do not
share the code except with the browser that is enrolling. Start the normal host
and use that code before it expires. Existing initialized installations do not
need to enroll again. Neither command starts or restarts a service.

To withdraw an approved web extension from a development root, stop the host and
run:

```sh
python3 scripts/withdraw_linux_extension.py /absolute/path/to/ROOT EXTENSION_ID
```

The command matches the explicit extension ID in the local approval (which the
server checks against the signed manifest), removes that approval, and advances the authentication
authorization generation, revoking existing sessions when the host next starts.
It retains the staged bundle and publisher history. Close every browser tab that
was open before withdrawal: a script already loaded in a tab can still execute.
This local action does not revoke the publisher's key elsewhere.

The default plugin uses the released `WAIT_PRESENT` / `GET_FRAME` driver ABI and
borrows DMA-BUF descriptors through capture v1.5, without a capture-side copy or
a driver upgrade. It does not promise immutable retained pixels.
`gpu_snapshot:true` requests private GPU copies and retained-frame interfaces.
The experimental `driver-read-leases` build feature must be enabled on both
plugin and helper to use the new driver lease ABI; only that mode requires a
matching driver upgrade and delays buffer retirement while a frame is held.

The Linux launch profile explicitly inherits the desktop identity and session
variables (`HOME`, `USER`, `LOGNAME`, `PATH`, `DISPLAY`, `WAYLAND_DISPLAY`,
`XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `XAUTHORITY`, `XDG_SESSION_TYPE`,
`LANG`, `LC_ALL`). The launch plugin snapshots only this startup allowlist;
configured `environment` values take precedence. Loader overrides and unrelated
host credentials are not implicitly inherited. Steam needs this context to
forward a launch to the logged-in client. A failed Steam launcher is reported as
an unsuccessful exit rather than a running game.
