# Amaterasu

A Rust-first streaming host being built around independently updateable libraries.
The shell loads and supervises components; product behavior belongs in those
components. Vibeshine is the behavior reference, not a source tree to copy.

**Current state: a working development foundation, not a streaming host yet.**
The runtime supports permission-scoped requests, bounded events, and ordered
hooks. The shell loads independently built native libraries, verifies signed
packages, probes their lifecycle in a subprocess, and recovers failed activation.
Persistent state, schema-validated configuration, session/resource coordination,
a scalable manual application catalog, tick-driven application preparation and
process-tree cleanup, diagnostics, local Steam discovery, authentication, and a
protected loopback management API are implemented as separate libraries. The
echo module remains a test fixture.
Software Opus encoding and UDP media delivery are independently packaged. A
shared Rust protocol library handles RTSP/SDP parsing, audio/video packetization,
FEC, ENet reliability and media/control encryption. An authenticated streaming session coordinator still
needs to connect capture, encoding, transport and control traffic.

## Build and verify

Use Rust 1.89 or newer and Python 3.10 or newer. The local implementation was
validated on macOS ARM64 with Rust 1.98.1. The whole workspace also passes
`cargo check` for `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu`.
Windows/Linux CI is configured but has not been executed from this checkout;
cross-target compilation does not establish runtime or hardware compatibility.
See [cross-compilation setup](docs/cross-compilation.md) for the target C tools
needed by TLS cryptography and bundled Opus on a macOS development host. Building
Opus also requires CMake and a compatible C compiler. The independent Python
cryptographic checks in `test_native_media.py`, `test_control_transport.py` and `test_nvhttp.py` require the
`cryptography` package in the test environment.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
python3 scripts/test_component_updates.py
python3 scripts/test_platform_modules.py
python3 scripts/test_native_media.py
python3 scripts/test_host_service.py
python3 scripts/test_management_api.py
python3 scripts/test_web_bundles.py
python3 scripts/test_management_domains.py
python3 scripts/test_nvhttp.py
python3 scripts/test_launch_workflow.py
```

The integration script uses temporary directories and a fresh development signing
key. It verifies module replacement without changing the shell binary, failed
health/stall recovery, interrupted activation, historical selection, tamper
rejection, and unsigned mode. It does not install services or touch hardware.
The platform integration script exercises persistence across process restarts,
configuration validation, caller namespaces, session lease ownership, catalog
CRUD, diagnostic retention, and controlled child-process launch/termination using
the actual dynamically loaded libraries. The launch workflow integration also
checks ordered preparation, reverse undo, and failed-preparation rollback.

## Try a component

```sh
cargo build --workspace --locked
cargo run -p amaterasu-release -- init .local/host
cargo run -p amaterasu-release -- pack .local/host target/debug .local/release-1 1
cargo run -p amaterasu-release -- stage .local/host .local/release-1
cargo run -p amaterasu -- probe .local/host '{"op":"request","service":"example.echo/1","payload":{"hello":"world"},"timeout_ms":1000,"operation_id":"example.first"}'
```

The probe is short-lived. `amaterasu serve .local/host` keeps modules alive and
accepts JSON control messages on stdin; see the [host protocol](crates/host/README.md).
The publisher refuses to
replace an existing development identity or package directory. Use a new release
directory and sequence for each package.

Use `pack-platform` instead of `pack` to package all current product libraries with the
runtime. The probe also accepts an array of control messages to execute
several operations in the same process. Each mutation should have a distinct
`operation_id`. Module protocols are documented in their READMEs:
[state](modules/state/README.md), [configuration](modules/configuration/README.md),
[session coordination](modules/session/README.md), [catalog](modules/catalog/README.md),
[applications](modules/application/README.md), [diagnostics](modules/diagnostics/README.md),
[Steam discovery](modules/steam/README.md), [authentication](modules/authentication/README.md),
and [management HTTP](modules/management/README.md).

The application service executes explicit programs under the host's account and
requires a trusted service grant. Its children are terminated when the short-lived
probe exits. Catalog storage supports up to 100,000 entries using atomic sharded
collections; 10,000-entry persistence and pagination are tested. Diagnostics is
in-memory. These limits and the remaining application lifecycle parity gaps are
documented in the module READMEs.

## Code map

| Location | Responsibility |
| --- | --- |
| `crates/abi` and `sdk/include` | Draft Rust/C bootstrap ABI and memory ownership |
| `crates/identity` | Pairing-owned native TLS signing and committed client trust interface |
| `crates/media` | Draft C interfaces for borrowed CPU frames and encoded packets |
| `crates/gamestream` | Bounded RTSP/SDP parsing, audio/video packetization, FEC and media encryption |
| `crates/contracts` | Serialized control messages and grants |
| `crates/runtime/src/broker` | Separate request, event, hook, and configuration subsystems |
| `crates/runtime/src/ffi.rs` | Runtime ABI boundary |
| `crates/module-sdk` | Ordinary Rust module lifecycle and shared ABI adapters |
| `crates/host` | Native loading and bounded development supervision |
| `crates/package` | Metadata, signatures, artifact verification, staging, atomic records |
| `crates/storage` | Shared durable-file primitives without release-signing dependencies |
| `crates/release` | Local development publishing and staging commands |
| `modules/state` | Persistent namespaces, revisions, compare-and-set and paginated snapshots |
| `modules/configuration` | Caller-owned schemas, validation and asynchronous state persistence |
| `modules/session` | Session lifecycle, shared/exclusive resource leases and snapshots |
| `modules/catalog` | Durable manual entries, stable IDs, search and revision-bound pagination |
| `modules/application` | Caller-owned process groups/Jobs, tick-driven preparation, reverse undo, status and termination |
| `modules/diagnostics` | Caller-scoped bounded records, retention cursors and counters |
| `modules/steam` | Incremental local library discovery and bounded snapshot pagination |
| `modules/authentication` | Durable password credentials, sessions, scoped tokens and revocation |
| `modules/nvhttp` | Moonlight HTTP/TLS pairing, application lists and certificate-owned application/RTSP launch |
| `modules/management` | Explicit loopback HTTP routes, origin/framing checks and token authorization |
| `modules/capture-wgc` | Exact-monitor Windows Graphics Capture with joined worker ownership |
| `modules/audio` | Windows WASAPI loopback with bounded borrowed PCM delivery |
| `modules/audio-encode` | Caller-owned software Opus encoding, packet queues and drain |
| `modules/capture-dxgi` | Caller-owned Windows desktop duplication and CPU readback |
| `modules/encode` | Windows hardware H.264 encoding and bounded packet delivery |
| `modules/video` | Asynchronous capture/encoder coordination and owned output queues |
| `modules/control-transport` | Owned ENet connections, authenticated control messages and native delivery |
| `modules/stream-session` | Authenticated media lifecycle, native forwarding, shared capture leases and confirmed cleanup |
| `modules/rtsp` | Encrypted RTSP tickets, strict negotiation and delegated media lifecycle |
| `fixtures/stream-source` | Synthetic video/PCM sources for signed session composition tests |
| `modules/media-transport` | Owner-scoped UDP delivery, bounded audio/video queues, FEC and optional media encryption |
| `fixtures/media-probe` | Signed-library native binding integration without opening hardware |
| `fixtures/echo` | Replaceable module and failure-injection fixture |

See [architecture](docs/architecture.md), [protocol](docs/control-protocol.md),
[parity status](docs/parity.md), and [dependency decisions](docs/dependencies.md).

The [frontend](frontend/README.md) builds without a runtime framework.
[Signed web bundles](docs/web-bundles.md) activate on page refresh independently of native packages.

Catalog-backed launch coordination lives in [the launch module](modules/launch/README.md).
Keyboard/mouse injection and temporary driver-owned monitors are separate optional
[input](modules/input/README.md) and [virtual-display](modules/virtual-display/README.md)
libraries. Their startup policies default to disabled; local tests do not inject
input or change displays.

Windows [DXGI capture](modules/capture-dxgi/README.md),
[hardware H.264 encoding](modules/encode/README.md), and
[video coordination](modules/video/README.md) are separate, disabled-by-default
libraries. They exchange borrowed frames and packets through the draft
[native media ABI](docs/media-interfaces.md). Signed-library tests cover binding
and ownership; real Windows capture-to-encode hardware qualification is pending.

[WGC capture](modules/capture-wgc/README.md) is an allowlisted alternative in video
settings. [WASAPI audio](modules/audio/README.md) captures an explicit endpoint
without changing system defaults. Both are disabled by default and await physical
Windows qualification. The management library exposes scoped media discovery;
video supports owned keyframe requests and graceful output draining.

The [Moonlight HTTP/TLS adapter](modules/nvhttp/README.md) implements Gen7 pairing
and server information, with disabled-by-default listeners. Its authenticated
application list reads names and stable IDs from one explicitly authorized catalog;
the development profile selects the management library. It delegates TLS
signing to the pairing library through the native identity interface; private keys
stay in that library. `scripts/test_nvhttp.py` exercises the actual HTTP transcript,
TLS client proof, durable trust and application lists across restarts. Optional
streaming configuration enables exported application launch and owned cancellation,
with encrypted RTSP and UDP discovery delegated to the media libraries.
`scripts/test_public_launch.py` checks that composition with a controlled child
and synthetic video. Optional session input policy routes authenticated keyboard
and mouse packets through the leased input library; `scripts/test_stream_input.py`
verifies dispatch and release through an in-memory provider. Display optimization,
broader media negotiation, resume, captured-monitor absolute input and real
client/hardware playback remain parity gates.
