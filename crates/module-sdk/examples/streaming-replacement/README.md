# Streaming replacement qualification fixture

This independently built native fixture implements library `/2`, streaming-policy
`/2` and executor `/1` contracts using generated records and an exact certificate
check. Its executor advertises versioned limiter-target attestation only in the
limiter test modes and checks reservation identity against the running ticket. It
is separate from the snapshot library, static policy and other executor examples.
Its failure and mismatch modes are test fixtures, not product settings.
It installs no service grants and does not execute applications.

The Linux NVHTTP composition test loads locally built protocol and fixture
artifacts. It invokes the production NVHTTP router, pairing authority, Launch,
RTSP, lifecycle and runtime; persistent state and hardware backends are synthetic.
By default its only listener is RTSP on an ephemeral loopback port, and TLS
transport trust is injected. Setting `AMATERASU_COMPOSITION_USE_TLS=1` also
uses an ephemeral loopback NVHTTP listener and performs a client-certificate TLS
handshake with the actual pairing identity. Hardware, input and stream services
remain synthetic; this is not a feedback-retirement qualification. Build each
standalone Cargo workspace in its own target directory to avoid artifact collisions. For example, from the repo
root with `out` pointing to task-owned scratch space:

For an additional selected-input check, build `amaterasu-input` with the
`qualification-fake-backend` feature into the protocol target and set
`AMATERASU_COMPOSITION_REAL_INPUT=1`. The input library then runs with a fake,
device-free backend and a fixture configuration service. Stream and media
services still use fixture implementations.

```sh
CARGO_TARGET_DIR="$out/protocol" cargo build --offline --locked -p amaterasu-runtime -p amaterasu-pairing -p amaterasu-launch -p amaterasu-rtsp -p amaterasu-session-lifecycle
CARGO_TARGET_DIR="$out/library" cargo build --offline --locked --manifest-path crates/module-sdk/examples/application-library-v2/Cargo.toml
CARGO_TARGET_DIR="$out/policy" cargo build --offline --locked --manifest-path crates/module-sdk/examples/streaming-policy-provider/Cargo.toml
CARGO_TARGET_DIR="$out/executor" cargo build --offline --locked --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
CARGO_TARGET_DIR="$out/replacement" cargo build --offline --locked --manifest-path crates/module-sdk/examples/streaming-replacement/Cargo.toml
AMATERASU_COMPOSITION_ARTIFACT_DIR="$out/protocol/debug" \
AMATERASU_LIBRARY_ARTIFACT="$out/library/debug/libexample_application_library_v2.so" \
AMATERASU_STREAMING_POLICY_ARTIFACT="$out/policy/debug/libexample_streaming_policy_provider.so" \
AMATERASU_EXECUTOR_ARTIFACT="$out/executor/debug/libexample_application_library_provider.so" \
AMATERASU_REPLACEMENT_ARTIFACT="$out/replacement/debug/libexample_streaming_replacement.so" \
CARGO_TARGET_DIR="$out/protocol" cargo test --offline --locked -p amaterasu-nvhttp real_selected_composition -- --ignored
```
