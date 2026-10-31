# Negotiated audio artifact proof

One unchanged consumer library runs against two separately compiled, independent
synthetic providers. The providers exercise the negotiated table and BUSY retry;
their bytes are test markers, not encoded audio. This is an artifact-level ABI
proof, not a test of host package loading, all encoder lifecycle operations,
client protocol interoperability, or actual AC-3/E-AC-3 encoding.

On Linux, from the repository root:

```sh
CARGO_TARGET_DIR=/tmp/amaterasu-audio-fixtures cargo build --offline --manifest-path fixtures/audio-provider-conformance/Cargo.toml -p audio-fixture-ac3 -p audio-fixture-eac3 -p audio-fixture-consumer
AUDIO_FIXTURE_CONSUMER=/tmp/amaterasu-audio-fixtures/debug/libaudio_fixture_consumer.so \
AUDIO_FIXTURE_AC3=/tmp/amaterasu-audio-fixtures/debug/libaudio_fixture_ac3.so \
AUDIO_FIXTURE_EAC3=/tmp/amaterasu-audio-fixtures/debug/libaudio_fixture_eac3.so \
CARGO_TARGET_DIR=/tmp/amaterasu-audio-fixtures cargo test --offline --manifest-path fixtures/audio-provider-conformance/Cargo.toml -p audio-fixture-harness -- --ignored
```
