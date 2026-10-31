# Broker-routed provider fixture

This independently built module registers `example.library.provider/1`. It
implements discovery, artwork, and synthetic lifecycle using the SDK adapter.
It exists to exercise the provider protocol, not to launch a game. The single
fixture operation table is in memory; after a restart lookup and stop return
`unknown`. A production provider must persist and reconcile effects and prove
retirement; this fixture cannot demonstrate those properties.

Build and test from the repository root:

```sh
cargo build --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
cargo test --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
```

The host must install and select an authorized signed set, approve the provider's
registration grant, and authorize the selected library to call this exact
service. Set the provider's startup settings to
`{"library_caller":"amaterasu.library","allowed_initiators":["example.consumer"]}`
using the actual configured component identities. Both names are checked against
broker-provided immediate caller and initiator. An empty initiator set denies
all calls. The library's own startup policy must independently permit the
initiator and bind the provider key to this service. A request payload cannot
choose a service or identity. Native modules are trusted; grants do not sandbox
native code.

A direct provider discovery payload is `{"action":"discover","cursor":null,
"limit":20}`. Successful responses contain `ok`, a provider revision string,
entries and `next_cursor`. Clients must pin revisions across pages and reject
inconsistent scans. Artwork uses `{action:"artwork",reference:{id,revision},
offset}` and returns base64 bytes with reference, MIME, offset and total length;
check each against the pinned stream. Limits are 32 rows, 8 KiB per artwork
chunk, 16 MiB per object, and 12,000 encoded bytes per response.

The lifecycle actions `launch`, `lookup`, and `stop` carry the stable lifecycle
`operation_id`; broker admission uses its own stable retry identity. A real
provider must reject changed arguments for a reused ID, make retries idempotent,
and return `unknown` when it cannot establish an effect. This adapter dispatches
these actions, but the current launch coordinator has no generic provider
integration or retirement proof protocol. Successful synthetic fixture states
are not evidence of launch or cleanup. The tests use a simulated host ABI; they
do not establish signed-host installation or end-to-end gameplay.
