# Cross-compilation from Apple Silicon macOS

The commands below were verified with Rust 1.98.1, Apple Clang 21.0.0,
Zig 0.14.1, xwin 0.6.5, and the repository's locked ring 0.17.14/rustls
0.23.44 dependencies, plus `opusic-sys` 0.7.5 bundling Opus 1.6.1. They build
real target C and assembly code for ring and Opus; no
cryptographic feature is disabled and no substitute headers are used.

`cargo check --all-targets` checks Rust library, binary, and test targets and
runs native dependency build scripts. It does **not** link the final Amaterasu
executables or DLLs, run target tests, qualify target TLS behavior, or exercise
Windows capture/encode/input/audio hardware. Native Windows and Linux builds
and integration tests remain required before release.

## Temporary tools

These paths keep cross artifacts separate from native signed-host builds. Set
the existing isolated Rust toolchain environment and install its Rust targets:

```sh
export CARGO_HOME=/private/tmp/amaterasu-toolchain/cargo
export RUSTUP_HOME=/private/tmp/amaterasu-toolchain/rustup
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_TARGET_DIR=/private/tmp/amaterasu-cross/target
export ZIG_GLOBAL_CACHE_DIR=/private/tmp/amaterasu-cross/zig-cache
rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
mkdir -p /private/tmp/amaterasu-cross
```

Download the official Apple Silicon Zig release and verify its published
SHA-256 before extracting. The checksum came from the official
[Zig release index](https://ziglang.org/download/index.json).
[Zig documents its C cross-compiler support](https://ziglang.org/learn/overview/#cross-compiling-is-a-first-class-use-case).

```sh
curl --location --fail --show-error \
  https://ziglang.org/download/0.14.1/zig-aarch64-macos-0.14.1.tar.xz \
  -o /private/tmp/amaterasu-cross/zig.tar.xz
python3 - <<'PY'
import hashlib
import tarfile
archive = '/private/tmp/amaterasu-cross/zig.tar.xz'
expected = '39f3dc5e79c22088ce878edc821dedb4ca5a1cd9f5ef915e9b3cc3053e8faefa'
assert hashlib.sha256(open(archive, 'rb').read()).hexdigest() == expected
with tarfile.open(archive) as package:
    package.extractall('/private/tmp/amaterasu-cross', filter='data')
PY
```

The extraction commands require Python 3.12 or newer. The tested host used
Python 3.14. Wrappers below only adapt compiler command-line syntax; they do
not modify sources or compiler checks.

## Bundled Opus build tools

The workspace now includes bundled Opus. Its build script uses CMake and selects
Ninja when available. Install the tested versions in a temporary virtual environment:

```sh
python3 -m venv /private/tmp/amaterasu-cross/build-tools
/private/tmp/amaterasu-cross/build-tools/bin/python -m pip install \
  cmake==3.31.10 ninja==1.13.0
export PATH="/private/tmp/amaterasu-cross/build-tools/bin:$PATH"
```

These are the PyPI CMake and Ninja distributions; the tested Ninja binary reports
`1.13.0.git.kitware.jobserver-pipe-1`. This setup does not patch Opus or replace it
with a system codec. The locked Rust wrapper uses the bundled C sources and
keeps hardening, stack protection, fortify and runtime SIMD configuration enabled.
The wrapper itself requests static-library compiler probes for cross compilation;
we do not override compiler success results or attempt to run target executables.

## Linux GNU target

Zig supplies the target libc headers. Its target spelling omits Rust's
`unknown` vendor component. Put `-target` **after** forwarded arguments so it
overrides cc-rs's automatically supplied Rust target spelling.

```sh
cat > /private/tmp/amaterasu-cross/linux-cc <<'SH'
#!/bin/sh
exec /private/tmp/amaterasu-cross/zig-aarch64-macos-0.14.1/zig cc "$@" -target x86_64-linux-gnu
SH
cat > /private/tmp/amaterasu-cross/zig-ar <<'SH'
#!/bin/sh
exec /private/tmp/amaterasu-cross/zig-aarch64-macos-0.14.1/zig ar "$@"
SH
chmod +x /private/tmp/amaterasu-cross/linux-cc /private/tmp/amaterasu-cross/zig-ar
export CC_x86_64_unknown_linux_gnu=/private/tmp/amaterasu-cross/linux-cc
export AR_x86_64_unknown_linux_gnu=/private/tmp/amaterasu-cross/zig-ar
cargo check --locked --offline -p amaterasu-nvhttp --all-targets \
  --target x86_64-unknown-linux-gnu
cargo clippy --locked --offline -p amaterasu-nvhttp --all-targets \
  --target x86_64-unknown-linux-gnu -- -D warnings
```

## Windows MSVC target

The macOS compiler alone lacks the Microsoft CRT/SDK headers. Obtain genuine
headers and libraries through [xwin](https://github.com/Jake-Shadle/xwin).
The official 0.6.5 Apple Silicon asset and its adjacent `.sha256` file were
verified; newer releases inspected during this work lacked macOS binaries.

```sh
curl --location --fail --show-error \
  https://github.com/Jake-Shadle/xwin/releases/download/0.6.5/xwin-0.6.5-aarch64-apple-darwin.tar.gz \
  -o /private/tmp/amaterasu-cross/xwin.tar.gz
python3 - <<'PY'
import hashlib
import tarfile
archive = '/private/tmp/amaterasu-cross/xwin.tar.gz'
expected = '21acd1e35d23b72efc8c47cbeda5028989f98089174b8c87074f892b65adbc01'
assert hashlib.sha256(open(archive, 'rb').read()).hexdigest() == expected
with tarfile.open(archive) as package:
    package.extractall('/private/tmp/amaterasu-cross', filter='data')
PY
/private/tmp/amaterasu-cross/xwin-0.6.5-aarch64-apple-darwin/xwin \
  --accept-license --cache-dir /private/tmp/amaterasu-cross/xwin-cache \
  --arch x86_64 --timeout 30 splat \
  --output /private/tmp/amaterasu-cross/windows-sdk
```

`--accept-license` accepts Microsoft's SDK/CRT terms. The verified download
selected Visual Studio manifest `17.14.40+37628.2`, CRT headers `14.44.35220`,
and SDK package `10.0.26100.15`. xwin caches the downloaded manifest and
packages; future default manifest resolution may select newer versions. For
an exact replay, retain the verified cache and manifest instead of assuming
Microsoft's moving release channel is immutable. Temporary SDK output was
approximately 629 MiB, with about 1 GiB of download/unpack cache.

Apple Clang accepts the MSVC target triple generated by cc-rs. Supply the
actual SDK include directories and use Zig's `lib` implementation to create
COFF archives. ring's published package provides its preassembled Windows
objects; no local NASM installation was needed.

```sh
cat > /private/tmp/amaterasu-cross/zig-lib <<'SH'
#!/bin/sh
exec /private/tmp/amaterasu-cross/zig-aarch64-macos-0.14.1/zig lib "$@"
SH
chmod +x /private/tmp/amaterasu-cross/zig-lib
export CC_x86_64_pc_windows_msvc=/usr/bin/clang
export AR_x86_64_pc_windows_msvc=/private/tmp/amaterasu-cross/zig-lib
export CFLAGS_x86_64_pc_windows_msvc='-isystem /private/tmp/amaterasu-cross/windows-sdk/crt/include -isystem /private/tmp/amaterasu-cross/windows-sdk/sdk/include/ucrt -isystem /private/tmp/amaterasu-cross/windows-sdk/sdk/include/shared -isystem /private/tmp/amaterasu-cross/windows-sdk/sdk/include/um'
cat > /private/tmp/amaterasu-cross/zig-ranlib <<'SH'
#!/bin/sh
exec /private/tmp/amaterasu-cross/zig-aarch64-macos-0.14.1/zig ranlib "$@"
SH
cat > /private/tmp/amaterasu-cross/zig-rc <<'SH'
#!/bin/sh
exec /private/tmp/amaterasu-cross/zig-aarch64-macos-0.14.1/zig rc "$@"
SH
chmod +x /private/tmp/amaterasu-cross/zig-ranlib /private/tmp/amaterasu-cross/zig-rc
cat > /private/tmp/amaterasu-cross/opus-windows.cmake <<'CMAKE'
set(CMAKE_SYSTEM_NAME Windows)
set(CMAKE_SYSTEM_PROCESSOR AMD64)
set(CMAKE_RC_COMPILER /private/tmp/amaterasu-cross/zig-rc)
set(CMAKE_AR /private/tmp/amaterasu-cross/zig-ar)
set(CMAKE_RANLIB /private/tmp/amaterasu-cross/zig-ranlib)
set(CMAKE_C_COMPILER /usr/bin/clang)
set(CMAKE_C_COMPILER_TARGET x86_64-pc-windows-msvc)
CMAKE
export CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc=/private/tmp/amaterasu-cross/opus-windows.cmake

cargo check --locked --offline -p amaterasu-nvhttp --all-targets \
  --target x86_64-pc-windows-msvc
cargo clippy --locked --offline -p amaterasu-nvhttp --all-targets \
  --target x86_64-pc-windows-msvc -- -D warnings
```

The CMake Windows toolchain is needed for Opus, while ring's cc-rs build still
uses `AR_x86_64_pc_windows_msvc=zig-lib`. Apple Clang has a GNU command-line
frontend even when emitting MSVC COFF objects, so CMake generates `ar qc` and
`ranlib` commands. Supplying `zig-lib` as **CMake's** archiver fails because `qc`
is interpreted as an input file. `zig-ar` and `zig-ranlib` implement the expected
syntax and produce the archive without substituting target objects. Zig's real
resource compiler satisfies CMake's Windows RC tool lookup. If an earlier failed
configure cached different tools, remove only that temporary Opus `out/build`
directory before retrying; do not disable compiler checks.

The resulting Opus encoder objects were inspected as x86-64 ELF on Linux and
AMD64 COFF on Windows; the Windows build installs `opus.lib`. CMake has not been
configured here to link final Windows executables. This remains a compile-only
check with the target execution and release qualification limits stated above.

Full workspace `--all-targets` checks and strict Clippy passed on both targets,
using `--workspace` in place of `-p amaterasu-nvhttp` with the same environment.
This includes bundled Opus, GameStream RTP/FEC and media crypto, and the managed
UDP media transport. Final checks were repeated after audio/video packetization,
crypto, and transport validation fixes were frozen.
Verification logs for this increment are:

- `/private/tmp/amaterasu-cross/media-workspace-linux.log`
- `/private/tmp/amaterasu-cross/media-workspace-windows.log`
- `/private/tmp/amaterasu-cross/media-workspace-linux-clippy.log`
- `/private/tmp/amaterasu-cross/media-workspace-windows-clippy.log`

The offline commands require the repository's dependencies to have been
fetched beforehand. No Cargo manifest, lockfile, or repository-wide compiler
configuration changes are needed for this setup.

The subsequent control-transport increment also passed full-workspace all-target
checks and strict Clippy on both target toolchains, including native control ABI,
ENet/control/RTSP crypto and the signed integration fixture. Logs are
`/private/tmp/amaterasu-cross/control-workspace-{linux,windows}.log` and
`/private/tmp/amaterasu-cross/control-workspace-{linux,windows}-clippy.log`.
These remain compile checks, not target runtime or final-release linkage proof.

The stream-session increment also passes full-workspace all-target checks and
strict Clippy on both targets, covering the managed lifecycle, native media plane,
packet-boundary opt-in and synthetic source fixture. Logs are
`/private/tmp/amaterasu-cross/stream-x86_64-unknown-linux-gnu{,-clippy}.log` and
`/private/tmp/amaterasu-cross/stream-x86_64-pc-windows-msvc{,-clippy}.log`.
Target execution, final linkage and hardware qualification remain separate gates.

The encrypted RTSP increment passes full-workspace all-target checks and strict
Clippy on both targets. This includes the armed session state, optional H.264
reference constraint and its Windows media-type readback, RTSP control/transport,
and the generic bounded startup-progress watchdog. Logs are
`/private/tmp/amaterasu-cross/rtsp-x86_64-unknown-linux-gnu{,-clippy}.log` and
`/private/tmp/amaterasu-cross/rtsp-x86_64-pc-windows-msvc{,-clippy}.log`.
Compilation still does not establish target runtime, hardware bitstream behavior
or final release linkage.

The UDP discovery increment also passes full-workspace all-target checks and
strict Clippy on Windows MSVC and Linux GNU. This covers the bounded UDP receive
path, RTSP token publication and session readiness gate. Logs are
`/private/tmp/amaterasu-cross/discovery-x86_64-unknown-linux-gnu{,-clippy}.log` and
`/private/tmp/amaterasu-cross/discovery-x86_64-pc-windows-msvc{,-clippy}.log`.
Loopback behavior is exercised on the host; target network/runtime qualification
remains separate.

The public-launch increment passes full-workspace all-target checks and strict
Clippy on both targets, including exported catalog ownership, managed launch
lookup, the HTTP launch lifecycle and TLS response-delivery reporting. Logs are
`/private/tmp/amaterasu-cross/public-launch-x86_64-unknown-linux-gnu{,-clippy}.log`
and `/private/tmp/amaterasu-cross/public-launch-x86_64-pc-windows-msvc{,-clippy}.log`.
These remain compile checks; controlled-process and socket integrations run on
the host and do not establish Windows/Linux target runtime or hardware behavior.

The session-input increment passes full-workspace all-target checks and strict
Clippy on Windows MSVC and Linux GNU. This covers portable key conversion, input
lease lookup, authenticated session dispatch and the synthetic input fixture.
Logs are `/private/tmp/amaterasu-cross/stream-input-{target}{-clippy}.log`.
The signed input integration uses an in-memory provider on the development host;
native SendInput/uinput delivery, permissions and desktop behavior remain target
runtime acceptance gates.
