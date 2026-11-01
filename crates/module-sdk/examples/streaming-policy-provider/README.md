# Selected streaming policy provider

This independently built example registers a startup-selected `/2` policy service.
It denies certificates unless a matching static rule exists. The Launch caller
and NVHTTP initiator require explicit broker grants and the `callers` map must
allow that exact pair; this example installs no grants. A fingerprint is an
assertion by the trusted protocol coordinator, not proof of TLS possession or
current pairing permission. Revocation must be enforced separately by the pairing
authority.

Settings contain `service`, `callers`, and at most 256 `rules`. Each rule has a
`certificates` set of lowercase SHA-256 fingerprints; a `policy` with the selected
`application`, opaque `entry_revision` and `policy_revision`, execution settings
and preparation; an object `config_overrides`; and optional
`application_display_settings`. Set `exclude_global_preparation` to true to
omit global preparation steps for the application; it defaults to false. Rules
for the same application, revision and
certificate must not overlap: ambiguous matches fail closed. Launch validates the
full returned policy and applies the accepted snapshot only after rechecking the
selected library and this service. Later ordinary policy updates govern new
admissions; security revocations are not frozen into this snapshot.

```sh
cargo build --offline --locked --manifest-path crates/module-sdk/examples/streaming-policy-provider/Cargo.toml
```

This example is a static authority; it does not itself implement pairing,
client authentication, revocation, OS process discovery or networking.
