# Neutral outbound provider fixture

This separately built SDK module supplies a small, in-memory input lease and
three neutral feedback entries: two negotiated schemas and one unapproved
schema. It is trusted test code and has no native input or hardware effects.
The test uses runtime-created caller endpoints, the stream-session consumer,
the real feedback pump, and an in-process synchronous transport.

From an approved worktree on ArchivedGames, run:

```sh
export CARGO_TARGET_DIR="$PWD/target"
export CARGO_BUILD_JOBS=2
cargo build --locked --offline -p amaterasu-runtime -p amaterasu-outbound-neutral
AMATERASU_RUNTIME_PLUGIN="$CARGO_TARGET_DIR/debug/libamaterasu_runtime.so" \
AMATERASU_OUTBOUND_NEUTRAL_PLUGIN="$CARGO_TARGET_DIR/debug/libamaterasu_outbound_neutral.so" \
  cargo test --locked --offline -p amaterasu-stream-session \
  loaded_neutral_provider_crosses_broker_session_and_real_queue -- --ignored
```

Both artifacts are built on the verified worktree volume. The surrounding
session dependencies and parent request are simulated; input lease and feedback
requests use the loaded module and runtime broker. This local ABI conformance
test does not sign, verify, stage, or install a host package and does not qualify
production peer negotiation, a real network transport or hardware behavior.
