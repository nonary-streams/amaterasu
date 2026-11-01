# Neutral outbound provider fixture

This separately built SDK module supplies a small, in-memory input lease and
three neutral feedback entries: two negotiated schemas and one unapproved
schema. It is trusted test code and has no native input or hardware effects.
The test uses runtime-created caller endpoints, the stream-session consumer,
the real feedback pump, and an in-process synchronous transport.

On the development host, from the repository root, the verified worktree target
and temporary directories and shared serial build lock can be used as follows:

```sh
export CARGO_TARGET_DIR="$PWD/.codex-outbound-target"
export TMPDIR="$PWD/.codex-outbound-tmp"
export CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
flock /home/chasep/sources/amaterasu/task-output/amaterasu-sdk-reset-build.lock \
  cargo build --offline -p amaterasu-runtime -p amaterasu-outbound-neutral
AMATERASU_RUNTIME_PLUGIN="$CARGO_TARGET_DIR/debug/libamaterasu_runtime.so" \
AMATERASU_OUTBOUND_NEUTRAL_PLUGIN="$CARGO_TARGET_DIR/debug/libamaterasu_outbound_neutral.so" \
  flock /home/chasep/sources/amaterasu/task-output/amaterasu-sdk-reset-build.lock \
  cargo test --offline -p amaterasu-stream-session \
  loaded_neutral_provider_crosses_broker_session_and_real_queue -- --ignored
```

Both artifacts are built on the verified worktree volume. The surrounding
session dependencies and parent request are simulated; input lease and feedback
requests use the loaded module and runtime broker. This local ABI conformance
test does not sign, verify, stage, or install a host package and does not qualify
production peer negotiation, a real network transport or hardware behavior.
