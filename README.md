# Amaterasu

**A gaming and streaming platform built to grow through plugins.**

Amaterasu's goal is simple: install the platform once, then add the features you
want through plugins. A new capability should be something you can enable from
a developer you trust, without installing another streaming host, switching
forks, or waiting for it to be added to the main application.

For gamers, that means choosing the features that suit their setup and having
them work together in one place. For developers, it means building and sharing
a feature independently, using the same SDK and APIs as the built-in plugins.
Game streaming is the starting point; a platform that the community can keep
extending is the larger purpose.

## One platform, many possibilities

Amaterasu builds on the experience behind Vibeshine and Vibepollo, with the goal
of bringing the full range of features from those projects, Sunshine, Apollo,
and the wider streaming ecosystem into one extensible platform. Users should be
able to combine the features they want through compatible plugins.

Development follows familiar Vibeshine behavior while bringing those
capabilities into a new Rust-first plugin architecture. Full feature parity
remains a long-term goal.
Existing plugins from other hosts need an Amaterasu implementation or an
appropriate adapter to work here.

The intended experience is straightforward:

- Add a library integration for another game library or launcher.
- Select a compatible plugin for a different capture method, encoder, or audio
  backend.
- Enable HDR processing, frame limiting, or different display behavior within
  the existing stream and session flow.
- Build and publish a new capability that others can use alongside their
  existing setup.

## Plugins are the platform

Plugins are how Amaterasu itself is built. Core features live in independently
built libraries, and several already have their own repositories and release
workflows. The small host shell verifies and loads components, manages their
lifecycle, and handles activation recovery. Streaming behavior belongs in the
plugins.

A plugin can **add a capability**, **extend another plugin**, or **replace an
implementation** behind a compatible API. Built-in consumers must use the same
versioned contracts and SDK adapters available to external developers. Being
bundled with Amaterasu should not give a feature a private integration path
that community developers cannot use.

For example, a frame-processing plugin should be invoked by the video pipeline
through the Processing SDK. It should not need to take over capture, choose the
encoder, or modify the host shell. The independently built
[RTX HDR plugin](modules/rtx-hdr/README.md) follows this model. Library, network,
capture, audio, and other provider contracts serve the same purpose in their
respective domains; [replacement tests](docs/plugin-substitution.md) check that
an unchanged consumer can work with another implementation.

Seamless integration is the design target. It still requires compatible
interfaces, declared capabilities and dependencies, and an approved package
composition. Independently released plugins do not imply arbitrary live swaps
or that every combination will work. Some features also need platform drivers
or vendor runtimes. Native plugins execute trusted code, so users must trust
their publishers; API grants are not a native-code sandbox.

## Where the project stands

**Amaterasu is in active development. The plugin platform and streaming paths
are implemented, but the complete feature set and seamless plugin experience
remain works in progress.**

Amaterasu is currently developed and tested on **Arch Linux**. **Windows support
is planned soon**; the next major push toward it is waiting for the next
generation of AI tools, with the aim of speeding up development and testing.
Existing Windows code and automated checks are groundwork for that support.

The repository includes a signed component loader, shared SDKs, provider
selection and replacement tests, authentication and management APIs, a web UI
with signed extensions, and independently built gaming and streaming modules.
Those modules cover game libraries and launch, Moonlight pairing and stream
coordination, capture and encoding, audio and input, physical and virtual
displays, remote sessions, frame limiting, and GPU processing.

An implementation or passing source test does not establish support on every
operating system, GPU, driver, or client. Some plugin boundaries and feature
behaviors are still incomplete. Use the focused documentation for scope and
remaining acceptance work:

- [Plugin substitution](docs/plugin-substitution.md): replacement boundaries,
  tests, and remaining interoperability work.
- [Configuration parity](docs/configuration-parity.md) and
  [execution plan](docs/configuration-execution-plan.md): the reference feature
  inventory and how behavior is verified.
- [Display parity](docs/vibeshine-display-parity.md): implemented behavior and
  known differences from Vibeshine.
- [Application-library ecosystem](docs/application-library-ecosystem.md):
  cross-project research and extension requirements.
- [Linux session acceptance](docs/linux-session-acceptance.md) and
  [Windows regression gates](docs/windows-regression-gates.md): platform
  qualification requirements.

The [parity ledger](docs/parity.md) also records earlier implementation
checkpoints. Read it alongside the focused reports rather than treating an
older checkpoint as the current support matrix.

## Build a plugin

Start with the [control-plane SDK](docs/control-plane-sdk.md) for module
lifecycle and services, then [stacking and extension points](docs/stacking-and-extension-points.md)
for hooks, events, and collaboration between plugins. The
[public SDK repository](https://github.com/nonary-streams/amaterasu-sdk) provides
the shared contracts and adapters used by independently built modules.

Use the contract for the capability you want to provide or consume:

| Area | Developer documentation |
| --- | --- |
| Capture and video encoding | [Capture SDK](docs/capture-plugin-sdk.md), [Encoder SDK](docs/encoder-plugin-sdk.md) |
| Frame processing and HDR | [GPU Processing SDK](docs/gpu-processing-sdk.md) |
| Audio | [Audio SDK](docs/audio-plugin-sdk.md) |
| Networking | [Network SDK](docs/network-plugin-sdk.md) |
| Game libraries | [Provider configuration](docs/application-library-provider-configuration.md), [replacement example](crates/module-sdk/examples/application-library-replacement/README.md) |
| Session behavior | [Session extensions](docs/session-extension-api.md), [remote sessions](docs/remote-session-plugin.md) |
| Settings and web UI | [Operation APIs](docs/plugin-apis.md), [Web Extension SDK](docs/web-extension-ui-sdk.md), [extension example](examples/web-extension/README.md) |

When an existing contract does not express a feature, extend the contract and
SDK so other developers can use it too. Keep implementation choices in the
provider, and verify replacement through an unchanged consumer.

## Build and verify

Use Rust 1.89 or newer and Python 3.10 or newer. Build requirements vary with
the selected platform plugins. Compilation and automated tests are separate
from runtime and hardware qualification.
See [cross-compilation setup](docs/cross-compilation.md) for the target C tools
needed by TLS cryptography and bundled Opus on a macOS development host. Building
Opus also requires CMake and a compatible C compiler. The independent Python
cryptographic checks in `test_native_media.py`, `test_control_transport.py` and `test_nvhttp.py` require the
`cryptography` package in the test environment.

Initialize all core module submodules before building the workspace. Each core
module has its own source repository, pinned here by commit and mounted where
the workspace builds it:

| Module | Repository | Mounted at |
| --- | --- | --- |
| Input | [amaterasu-input](https://github.com/nonary-streams/amaterasu-input) | `modules/input` |
| Audio capture | [amaterasu-audio](https://github.com/nonary-streams/amaterasu-audio) | `modules/audio` |
| Audio encoder | [amaterasu-audio-encode](https://github.com/nonary-streams/amaterasu-audio-encode) | `modules/audio-encode` |
| Video encoder and pipeline | [amaterasu-encode-video](https://github.com/nonary-streams/amaterasu-encode-video) | `modules/encode-video` |
| Virtual display capture | [amaterasu-capture-vkms](https://github.com/nonary-streams/amaterasu-capture-vkms) | `modules/capture-vkms` |
| Display | [amaterasu-display](https://github.com/nonary-streams/amaterasu-display) | `modules/display` |
| Remote session | [amaterasu-remote-session](https://github.com/nonary-streams/amaterasu-remote-session) | `modules/remote-session` |
| RTX HDR | [amaterasu-rtx-hdr](https://github.com/nonary-streams/amaterasu-rtx-hdr) | `modules/rtx-hdr` |
| Frame limiter | [amaterasu-frame-limiter](https://github.com/nonary-streams/amaterasu-frame-limiter) | `modules/frame-limiter` |
| Statistics | [amaterasu-stats](https://github.com/nonary-streams/amaterasu-stats) | `modules/stats` |

The shared crates under `crates/` and the headers under `sdk/` are published to
[amaterasu-sdk](https://github.com/nonary-streams/amaterasu-sdk) with
`python3 scripts/export_sdk.py PATH_TO_SDK_CHECKOUT`; each module repository
names the SDK commit it builds against in its `sdk-revision` file and checks
itself against that commit in its own continuous integration. Change a module
in its repository, push it, then advance the pin here. A change that needs a
new SDK interface lands here first, is exported to the SDK, and only then is
used by the module. Source extraction does not activate a package on an
installed host.

The capture provider is also a submodule. It has its own
lockfile and is validated separately from the parent workspace. Its helper-wire crate also builds the privileged helper; its shared wire
sources must match the broker library in this repository. Run
`python3 scripts/check_vkms_wire_sync.py` after changing either copy. Update and
test both copies together, publish the provider change, then advance the pin.

```sh
git submodule update --init --recursive
python3 scripts/check_vkms_wire_sync.py
cargo build --workspace --locked
cargo build --manifest-path modules/capture-vkms/helper-wire/Cargo.toml --locked
cargo test --manifest-path modules/capture-vkms/helper-wire/Cargo.toml --locked
cargo clippy --manifest-path modules/capture-vkms/helper-wire/Cargo.toml --all-targets --locked -- -D warnings
cargo fmt --manifest-path modules/capture-vkms/helper-wire/Cargo.toml --all --check
cargo build --manifest-path modules/capture-vkms/capture/Cargo.toml --locked
cargo test --manifest-path modules/capture-vkms/capture/Cargo.toml --locked
cargo clippy --manifest-path modules/capture-vkms/capture/Cargo.toml --all-targets --locked -- -D warnings
cargo fmt --manifest-path modules/capture-vkms/capture/Cargo.toml --all --check
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
| `crates/module-testkit` | Runs a module against the real control runtime, or a scripted one, inside a test |
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
| `modules/management` | Loopback HTTP and optional HTTPS routes, origin/framing checks and token authorization |
| `modules/capture-wgc` | Exact-monitor Windows Graphics Capture with joined worker ownership |
| `modules/audio` | Windows WASAPI loopback with bounded borrowed PCM delivery |
| `modules/audio-encode` | Caller-owned software Opus encoding, packet queues and drain |
| `modules/capture-dxgi` | Caller-owned Windows desktop duplication and CPU readback |
| `modules/encode-video/encode` | Windows hardware H.264 encoding and bounded packet delivery |
| `modules/encode-video/video` | Asynchronous capture/encoder coordination and owned output queues |
| `modules/control-transport` | Owned ENet connections, authenticated control messages and native delivery |
| `modules/stream-session` | Authenticated media lifecycle, native forwarding, shared capture leases and confirmed cleanup |
| `modules/rtsp` | Encrypted RTSP tickets, strict negotiation and delegated media lifecycle |
| `fixtures/stream-source` | Synthetic video/PCM sources for signed session composition tests |
| `modules/media-transport` | Owner-scoped UDP delivery, bounded audio/video queues, FEC and optional media encryption |
| `fixtures/media-probe` | Signed-library native binding integration without opening hardware |
| `fixtures/echo` | Replaceable module and failure-injection fixture |
| `sdk/examples/service-guard` | Example layer stacked on another module's service, with its policy fragment |

## Further reading

- [Architecture](docs/architecture.md), [control protocol](docs/control-protocol.md),
  and [dependency decisions](docs/dependencies.md).
- [Frontend development](frontend/README.md),
  [signed web bundles](docs/web-bundles.md), and the
  [web extension example](examples/web-extension/README.md).
- [Linux session setup](docs/linux-session-profile.md),
  [installer development](installer/README.md), and
  [local signed package updates](docs/local-package-updates.md).
- [Flow tracing](docs/flow-tracing.md) and
  [streaming latency diagnostics](docs/streaming-latency-diagnostics.md).

The component probe above is a development example. Installed production
updates use the signed package and activation flow, with compatible component
sets and the installation's existing trust policy. See
[local package updates](docs/local-package-updates.md) for that procedure.
