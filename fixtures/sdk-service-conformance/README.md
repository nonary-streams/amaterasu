# State SDK artifact conformance

This test loads one unchanged consumer module and runs the same key/value behavior
against the production State module and a separately built in-memory replacement.
Both use the real runtime ABI. The replacement deliberately implements only the
exercised key/value subset, is memory-only and bounded, and is not a production
State implementation. The test does not validate provider startup selection,
capability policy, persistent replacement storage, or all State operations.

On Linux, from the repository root (use platform library names elsewhere):

```sh
CARGO_TARGET_DIR=/tmp/amaterasu-state-conformance cargo build --offline -p amaterasu-runtime -p amaterasu-state
CARGO_TARGET_DIR=/tmp/amaterasu-state-fixtures cargo build --offline --manifest-path fixtures/sdk-service-conformance/Cargo.toml -p sdk-state-consumer-fixture -p sdk-state-replacement-fixture
AMATERASU_CONFORMANCE_RUNTIME=/tmp/amaterasu-state-conformance/debug/libamaterasu_runtime.so \
AMATERASU_CONFORMANCE_STATE=/tmp/amaterasu-state-conformance/debug/libamaterasu_state.so \
AMATERASU_CONFORMANCE_CONSUMER=/tmp/amaterasu-state-fixtures/debug/libsdk_state_consumer_fixture.so \
AMATERASU_CONFORMANCE_REPLACEMENT=/tmp/amaterasu-state-fixtures/debug/libsdk_state_replacement_fixture.so \
CARGO_TARGET_DIR=/tmp/amaterasu-state-fixtures cargo test --offline --manifest-path fixtures/sdk-service-conformance/Cargo.toml -p sdk-service-conformance -- --ignored --nocapture
```

It compares the consumer artifact's SHA-256 before and after each fresh runtime
composition, verifies caller-scoped state, revision conflicts and denied delegated
calls, broker capacity recovery, cancellation before provider execution, and consumer shutdown with queued work. Cancellation is not a
rollback guarantee for work that has already executed. Artifacts are trusted test
modules; runtime grants are not a native-code sandbox.
