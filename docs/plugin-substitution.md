# SDK-driven plugin substitution

A built-in consumer uses the same contract and SDK entry points as an external
consumer. The selected implementation lives behind that contract. A default
implementation is a product composition choice, with its own package identity,
grants and startup dependencies.

The network refactor supplies this boundary for management, NVHTTP, RTSP,
media/control delivery, Steam downloads, updater checks and UPnP. Their byte I/O,
DNS and interface operations use the Network SDK. HTTP/TLS framing, GameStream
packetization/encryption/pacing, router protocols, library ownership and user
permissions remain with their domain implementations.

See [the Network SDK](network-plugin-sdk.md) for native contracts, configuration,
capabilities, ownership and platform qualification. Providers register a typed
native contract; the runtime rejects a mismatched interface family before
calling its factory. A table with matching size and major alone is insufficient.
Legacy unsafe native bindings remain available for existing documented contracts.

## Composition and lifetime

The default component is `amaterasu.network`, providing `amaterasu.network/1`.
Consumers can select a different service through trusted `network_provider`
startup settings. Their signed composition must grant native use of that service
and keep its component loaded through every consumer worker and socket lifetime.
Changing a payload cannot select another provider or widen these grants.

The shell remains responsible for loading and lifecycle; it has no networking
protocol logic. There is no silent local-socket fallback when a selected provider
is unavailable or lacks a required capability. Optional batching can fall back
to ordinary sends through the same selected provider, preserving datagram
boundaries and the accepted-prefix retry contract.

Bootstrap repository retrieval is an explicit exception: the publisher must be
able to retrieve verified packages before provider plugins are loaded. It already
accepts an injected `RepositoryTransport`; its approved-origin and signature
verification boundary stays independent of a live plugin runtime. Runtime
consumer plugins must not use that path to bypass provider selection.

## Library and orchestration

The library facade accepts startup-selected artwork routes with opaque reference
IDs and revisions through the shared Library SDK. Import ownership, initiating
principal, revision, MIME and total length remain pinned. Management artwork
views use a catalog ID; the browser cannot nominate a provider service or path.
Existing builtin compatibility adapters retain their old client contracts.

Stream orchestration accepts a trusted `providers` map for session, video,
audio capture/encoding, media transport and control. Each stream pins that map
through native binding, cleanup and restart recovery. Custom native roles must
acknowledge their expected contract before the table is used. Refer to the
stream-session README for exact keys and the required delegated call grants.

The selected `amaterasu.input/1` provider may advertise ordered mixed-event
batches per lease. The shared [input batch contract](input-batch-contract.md)
and SDK describe capability negotiation and partial failure; an older provider
without that capability continues to receive homogeneous batches.

## Replacement acceptance

A boundary is qualified only when the ordinary consumer exercises an alternate
implementation without changing its source. The current tests include virtual
TCP listeners while an OS port is occupied, an in-memory HTTP endpoint with a
nonexistent hostname, alternate batch limits, explicit unsupported capabilities,
provider release, opaque third-party artwork and alternate orchestration roles.

The library artwork qualification loads separately compiled facade/provider
libraries. Network alternatives in the unit tests are independent native-table
implementations; qualification of a separately signed third-party network
package remains distinct from those tests and from the signed default network
composition.

## Remaining work

Artwork providers and NVHTTP/management consumers use the shared binary-transfer
SDK. See [artwork transfer](artwork-transfer.md) for the revision-pinned record,
provider grants, cleanup bounds, and remaining external Playnite protocol boundary.

The legacy encoded packet path accepts H.264/HEVC/AV1 and has a 16 MiB packet
limit. Generic format identifiers exist elsewhere, but full nonstandard-codec
orchestration and transport still need a negotiated output contract. WGC-specific
constant-rate pacing similarly needs a generic timing handshake. Neither adding
a network provider nor removing a codec validator would establish those semantics.

The default network backend's caller DNS timeout is bounded; final unload joins
uncancellable OS resolver calls and can wait on the operating system. Windows
batching is source-implemented but requires Windows compilation and runtime
qualification. Physical routers, remote throughput, and installed gameplay are
separate acceptance cases from loopback and fake-provider tests.
