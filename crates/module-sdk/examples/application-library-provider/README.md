# Broker-routed provider fixture

This independently built module registers `example.library.provider/1`. It
implements discovery, artwork, and synthetic lifecycle using the SDK adapter.
It exists to exercise the provider protocol, not to launch a game. The single
fixture operation table is in memory; after a restart old-incarnation
requests are rejected, while unknown IDs under the new incarnation remain unknown. A production provider must persist and reconcile effects and prove
retirement; this fixture cannot demonstrate those properties.

Build and test from the repository root:

```sh
cargo build --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
cargo test --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
```

The host must install and select an authorized signed set, approve the provider's
registration grant, and authorize the selected library to call this exact
service. Set provider startup settings, substituting the host's actual identities, for example:

```json
{"callers":{"amaterasu.library":{"initiators":["example.consumer"],"discovery":true,"artwork":true},"amaterasu.launch":{"initiators":["example.consumer","amaterasu.launch"],"lifecycle":true}}}
```

Every request checks both broker-attested immediate caller and initiator plus the
caller-specific capability. Missing callers, initiators and capabilities deny
access. The launch caller's initiator set includes the original consumer for
parented admission and the launch component for its later unparented monitoring
and cleanup. The library's own startup policy must independently permit the
initiator and bind the provider key to this service. A request payload cannot
choose a service or identity. Native modules are trusted; grants do not sandbox
native code.

A direct provider discovery payload is `{"action":"discover","cursor":null,
"limit":20}`. Successful responses contain `ok`, a provider revision string,
entries and `next_cursor`. Clients must pin revisions across pages and reject
inconsistent scans. Artwork uses `{action:"artwork_open",reference:{id,revision}}`
and returns a metadata-only binary-transfer offer. Register the native table
with `ProviderAdapter::register_artwork`, tick `reap_artwork`, and grant
`native_provide`/`native_use` under the same service alias. The provider returns
`ArtworkImage`; the adapter publishes its bytes through the shared SDK. The
standard policy uses 256 KiB chunks and caps each image at 16 MiB. The 12,000-byte
control response budget does not carry image bytes.

The launch owner first requests `{action:"identity"}` and pins the returned
random process incarnation. Lifecycle actions `launch`, `lookup`, and
`recover_stop` carry `operation_id`, `application_id`, and the pinned
`provider_incarnation`; `stop` also carries the pinned ticket. Broker admission
uses its own stable retry identity. Every observation echoes the pinned values;
`stopped` and `launch_failed` require a matching `fenced_stopped` assertion. The
provider scopes an operation by its authenticated immediate caller and operation
ID, allowing the broker initiator to change during launch-owned cleanup. A real
provider must reject changed arguments for a reused ID, fence delayed starts
before terminal assertions, and reconcile uncertain effects. This in-memory
fixture fences unknown operations only until its process exits. It cannot prove
cleanup across restart, and stale-incarnation requests fail before any effect.
The current launch coordinator must independently implement identity pinning,
recovery and retirement checks; these adapter checks alone do not supply that
integration. Successful synthetic states are not evidence of launch or cleanup.
Tests cover a simulated host ABI, parent-linked discovery through the real
runtime broker with a simulated library forwarder, and the actual launch module
with the real broker and synthetic provider. The launch test uses fake catalog
and session services and verifies lost-start recovery, detached monitoring and
stop, and a missing broker grant. These tests do not establish signed-host
installation, real application effects, or end-to-end gameplay.

The catalog-backed facade also accepts this provider for artwork without a
provider-specific dispatch branch. Configure the facade with:

```json
{
  "artwork": {
    "providers": {
      "vendor": {
        "service": "example.library.provider/1",
        "allowed_initiators": ["consumer"]
      }
    }
  }
}
```

The catalog's authenticated importer must be selected for the same `vendor`
provider alias. Its imported entry can carry this opaque reference:

```json
{
  "provider": "external",
  "source": "vendor",
  "reference": { "id": "fixture-cover", "revision": "fixture-v1" }
}
```

Provider alias and service selection come from trusted startup settings. Normal
broker grants must cover the facade, catalog, state and selected provider for
the initiating principal. The facade requests the SDK `describe` capability
before opening artwork, preserves reference/revision/MIME/total length,
and refuses unknown or unauthorized routes. The provider controls its resource
identity and returns the same shared SDK artwork contract as other providers.
The unchanged facade consumes the same binary-transfer SDK from this separately
compiled provider. See [artwork transfer](../../../../../docs/artwork-transfer.md).

The independently compiled provider/facade check is opt-in:

```sh
cargo build --offline -p amaterasu-library
cargo build --offline --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml
AMATERASU_LIBRARY_PLUGIN="$PWD/target/debug/libamaterasu_library.so" \
AMATERASU_PROVIDER_PLUGIN="$PWD/crates/module-sdk/examples/application-library-provider/target/debug/libexample_application_library_provider.so" \
cargo test --offline --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml \
  third_party_artwork_flows -- --ignored
```

Set the library paths for the platform and configured Cargo target directory.
The check exercises both accepted and explicitly unsupported artwork capability
responses through the same facade binary and a real broker.
