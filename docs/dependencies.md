# Dependency decisions

Direct dependencies are intentionally scoped. The workspace lockfile pins the
resolved graph; it includes target-specific and optional packages, so its package
count is not the same as the linked runtime footprint.

| Dependency | Scope | Reason |
| --- | --- | --- |
| `serde`, `serde_json` | Contracts, SDK, runtime, product libraries, metadata and CLI | Structured control/metadata parsing instead of a handwritten JSON parser |
| `libloading` | Shell only | Cross-platform native library ownership and symbol loading |
| `ed25519-dalek` | Package verifier and local publisher | Vetted Ed25519 implementation; strict signature verification |
| `sha2` | Package primitives | SHA-256 artifact identity |
| `getrandom` | Local publisher and shared storage temporary names | OS randomness without introducing a general random-number framework |
| `argon2` | Authentication library only | Argon2id password hashing with bounded, validated parameters; brings `password-hash` and `blake2` |
| `httparse` | Management library only | Established HTTP/1 header parsing; application code still enforces framing, origin, body and connection limits |
| `libc` | Unix application containment and Linux input | Platform types, process-group/wait/signal APIs, and Linux uinput ABI declarations without guessing native layouts |
| `windows-sys` | Windows application containment, display control, input and virtual-display adapters | Generated Win32 declarations for Job Objects, suspended process initialization, CCD display APIs, SendInput, and driver device/IOCTL interfaces; no platform runtime on other targets |

The ABI crate has no third-party dependencies. The initial runtime uses standard
collections and synchronization and does not bring in an async framework, external
message broker, database, or native media dependency.

Before adding a dependency, record its purpose, transitive footprint, platform
support, and why existing facilities are inadequate. This preference does not
justify custom cryptography, TLS, or codecs.

References: [Rust FFI reference](https://doc.rust-lang.org/reference/items/external-blocks.html),
[libloading](https://docs.rs/libloading/0.8.9/libloading/),
[ed25519-dalek](https://docs.rs/ed25519-dalek/2.2.0/ed25519_dalek/).

Pairing uses `rsa` (RSA signatures and PKCS#8 identity), `x509-cert` (bounded
certificate parsing/building), `aes` (the legacy client's AES-128 block transcript),
`sha2`, `subtle` (constant-time comparisons), and `rand_core` with OS randomness.
Their supporting DER/SPKI/PKCS crates and arithmetic dependencies are lockfile-pinned.
These implementations are confined to pairing; the core shell gains no pairing
protocol logic. TLS transport is still pending.

The frontend has one pinned development dependency, TypeScript, and no runtime
framework or CDN dependencies. Its compiled HTML/JavaScript/CSS ships as assets.
The management library reuses the existing package verifier for web signatures.

Launch coordination reuses `sha2` for bounded resource identities and `getrandom`
for per-instance operation namespaces. Input lease tokens and virtual-display owner
leases use the existing OS randomness dependency. These modules add no async runtime.

DXGI capture and Media Foundation encoding use the official `windows` 0.62 COM
bindings, scoped to Windows targets and the required D3D11/DXGI/MF/COM feature
families. This adds ten lockfile packages (`windows` and its core/string/result,
interface macros and support crates). Generated wrappers avoid guessed COM vtables
and manual reference-counting. The media ABI crate itself depends only on the
existing ABI crate. No FFmpeg, async executor, or image framework was introduced.

WGC and WASAPI reuse the existing Windows-only `windows` 0.62 bindings with
additional generated API features; they introduce no new external crate family.
Audio PCM types use only the existing ABI crate. Workers use standard-library
threads and bounded channels, without an async runtime or audio framework.

The Moonlight HTTP/TLS adapter uses rustls 0.23 with its ring provider and TLS 1.2
support. Application protocol, framing, lifecycle and identity delegation remain
Rust. Ring supplies vetted TLS cryptography and includes C/assembly; replacing
that with home-written cryptography would be a poor dependency tradeoff. Host RSA
signatures still use the existing pairing library's RustCrypto implementation.
The adapter reuses `x509-cert` and `pem-rfc7468` for accepted legacy v1 identities;
TLS possession signatures are verified by the selected rustls/ring algorithms.

Ring requires an appropriate C compiler and target headers. The current macOS
host builds and runs the TLS tests. A verified temporary Zig toolchain supplies
Linux target compilation; Apple Clang with genuine Microsoft CRT/SDK headers
supplies Windows MSVC checks. [Cross-compilation instructions](cross-compilation.md)
record the tools and checksums. These checks do not link release binaries or
qualify target runtime behavior.
`scripts/test_nvhttp.py` uses Python cryptography only as an independent test client;
it is not a deployed Amaterasu dependency.

The audio encoder pins `opusic-sys` 0.7.5 with only its `bundled` feature, building
the packaged libopus 1.6.1 statically into the independently signed module. It does
not probe for a system codec or load an ambient DLL. A small Rust ownership wrapper
contains the codec FFI; configuration, session authority, PCM conversion, buffering,
timing, backpressure and lifecycle remain Rust. The official codec supplies the
Opus bitstream implementation. This adds two locked crates: `opusic-sys` and its
`cmake` build helper, reusing the existing `cc` helper. CMake and a target C compiler
are build tools, not runtime services or installed codec prerequisites.

The encoded-audio C interface uses only the existing ABI dependency. The
GameStream protocol crate uses the Rust standard library, with no async runtime,
parser framework, sockets or native dependencies.
