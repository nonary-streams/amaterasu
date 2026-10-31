# Standalone read-only library replacement example

This independent Cargo project builds a `cdylib` that exports the module ABI and
registers `amaterasu.library/1`. It serves one static demonstration entry using
`list` and `resolve`. It does not execute a game or implement provider discovery,
artwork, lifecycle, persistence, or catalog import. The example is intended to
show the actual SDK registration and request lifecycle, not a complete product
replacement.

From the repository root:

```sh
cargo build --manifest-path crates/module-sdk/examples/application-library-replacement/Cargo.toml
cargo test --manifest-path crates/module-sdk/examples/application-library-replacement/Cargo.toml
```

The host must select a signed component providing this service in place of any
other registration for the same service, approve its exact identity and register
grant, grant consumers permission to call it, and configure its settings, e.g.
`{"allowed_callers":["example.consumer"]}`. Empty or missing settings deny all
callers. The module checks the broker-provided immediate caller and initiator are
equal and the caller is in that startup set. It deliberately rejects delegation;
production implementations need an explicit delegation policy. Payload identity
fields are rejected. Native modules are trusted code, not sandboxed by grants.

A separately built binary alone does not stage, sign, select, or activate a host
component. See `docs/approved-repositories.md` for development packaging and
policy requirements. The external-component packaging path does not itself
provide a dynamic replacement selector. Test coverage uses a simulated host ABI;
it does not establish signed-host installation or end-to-end streaming behavior.
