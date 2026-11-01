# Streaming replacement qualification fixture

This independently built native fixture implements both the library `/2` and
streaming-policy `/2` contracts using generated records and an exact certificate
check. It is separate from the snapshot-library and static-policy examples. Its
`stale` and `failed_preparation` modes are test fixtures, not product settings.
It installs no service grants and does not execute applications.

The Linux NVHTTP composition test loads locally built protocol and fixture
artifacts. It invokes the production NVHTTP router, pairing authority, Launch,
RTSP, lifecycle and runtime; persistent state and hardware backends are synthetic.
Its only listener is RTSP on an ephemeral loopback port. The test also needs
TLS transport trust injected by the fixture; it is not a TLS handshake or live
security-revocation qualification. Build each standalone Cargo workspace in its
own target directory to avoid artifact collisions. For example, from the repo
root with `out` pointing to task-owned scratch space:

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
