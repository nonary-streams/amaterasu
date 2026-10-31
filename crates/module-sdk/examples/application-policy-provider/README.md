# Independent application policy fixture

This separately built native fixture implements the versioned private policy
resolver SDK. It is trusted test code and returns a synthetic preparation plan;
it does not provide real applications, production policy, or NVHTTP preflight.
Its exact broker caller and initiator allowlist is deliberately limited to the
fixture composition.

From the repository root on Linux:

```sh
CARGO_TARGET_DIR="$PWD/target/policy-artifact" cargo build --offline --locked \
  --manifest-path crates/module-sdk/examples/application-policy-provider/Cargo.toml
AMATERASU_POLICY_ARTIFACT="$PWD/target/policy-artifact/debug/libexample_application_policy_provider.so" \
  cargo test --offline --locked \
  --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml \
  independently_built_policy_replaces_the_selected_resolver -- --ignored
```

The test uses the same Launch consumer against the in-process baseline resolver
and this independently built resolver through the real broker. Session, executor
and preparation effects are synthetic; no claim about a streaming protocol or a
real application follows from this replacement test.

The opt-in preview replacement check uses the same unchanged Launch consumer
and the same two selected policy providers, but stops before session, application
or executor effects. It does not establish NVHTTP or RTSP admission:

```sh
AMATERASU_POLICY_ARTIFACT="$PWD/target/policy-artifact/debug/libexample_application_policy_provider.so" \
  cargo test --offline --locked \
  --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml \
  independently_built_policy_replaces_resolver_during_readonly_preflight -- --ignored
```
