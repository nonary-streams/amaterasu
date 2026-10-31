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
inconsistent scans. Artwork uses `{action:"artwork",reference:{id,revision},
offset}` and returns base64 bytes with reference, MIME, offset and total length;
check each against the pinned stream. Limits are 32 rows, 8 KiB per artwork
chunk, 16 MiB per object, and 12,000 encoded bytes per response.

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
