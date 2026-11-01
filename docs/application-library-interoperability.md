# Optional library and execution interoperability

The first implementation increment from the [ecosystem plan](application-library-ecosystem.md)
adds `amaterasu.library/2` semantics and an opt-in `amaterasu.launch/2` endpoint.
These are contracts between selected plugins. They introduce no product behavior
in the shell and no library dependency on networking or packet delivery.

The legacy `amaterasu.library/1` and `amaterasu.launch/1` payloads are unchanged.
NVHTTP still uses its existing Catalog-backed compatibility path. A `/2` library
can use its own storage and IDs without providing Catalog records. A protocol
plugin that consumes `/2` owns any mapping to numeric client IDs. Implementations
can register a startup-selected service such as `example.library/2`; `/2` is an
explicit agreement, with no automatic fallback to `/1`.

## Implemented boundary

The SDK's `application_library::v2` contains public read types, validation and a
`LibraryAdapter` for independently implemented libraries. The adapter checks the
exact service, broker caller and initiator, request deadline, separate read and
launch-resolution permissions, field bounds and response size. An advertised
capability is not a grant. Native modules remain trusted code.

Supported library requests are:

```json
{"action":"describe"}
{"action":"list","query":{"text":""},"cursor":null,"limit":20}
{"action":"resolve","id":"opaque-library-id"}
{"action":"resolve_launch","id":"opaque-library-id","revision":"entry-v7"}
```

`describe` returns `{ok:true,capabilities:{version:2,features:[...]}}`. This profile
defines `library.search/1` and `library.resolve-launch/1`. The bounded feature set
can carry future versioned capabilities without changing existing read payloads;
unknown features are informational, never routing authority.
Consumers must select a supported version and check required capabilities.
Unsupported search or execution resolution fails explicitly. The declared
capabilities belong to this narrow profile; they do not promise edit, artwork,
source-management, asynchronous-action or change-feed APIs.

`list` returns `{ok:true,page:{revision,entries,next_cursor}}`. `resolve` returns
`{ok:true,entry}`. Each entry has an opaque ID and entry revision, name, optional
description/source identity, tags, portable store identities, installed state
(`yes`, `no`, `unknown`), readiness (`ready`, `unavailable`, `preparing`, `unknown`),
and an optional unavailable reason. Source-local identity, portable identity and
library identity are distinct. Unknown facts stay unknown; no command, path,
credential or arbitrary private JSON is part of the public projection.

A cursor is `{revision,token}`. Collection and entry revisions are independent
equality tokens. The library binds continuation tokens to the exact query and
authorized scope, rejects expired/stale tokens, and provides a consistent
snapshot. Consumers preserve cursors unchanged, reject duplicate entries across
pages, and restart after invalidation. Pages have at most 32 entries; a continuing
page must advance with at least one row. Every encoded response is at most
12,000 bytes. Implementations must choose page sizes that respect both bounds.

`resolve_launch` is a separate permission from browsing. It rechecks visibility,
readiness and the requested entry revision and returns only:

```json
{"ok":true,"target":{"id":"opaque-library-id","revision":"entry-v7","executor":"runner","application_id":"executor-owned-reference"}}
```

The executor key selects a route approved for this library. It is not a broker
service address. The executor reference must identify the intended execution
configuration; the executor remains responsible for validating readiness and
binding that configuration to the accepted operation. The library must not use
mutable display names or portable store IDs as execution authority.

The SDK adapter does not implement persistence, synchronization, overlay
precedence or authenticated human/device context. Those remain responsibilities
of the selected library and subsequent interoperability work. Broker caller and
initiator identify components, not people, devices or seats. Requests containing
invented identity/context fields are rejected by this profile.

## Selecting an execution composition

The bundled Launch plugin registers `/2` only when its startup `library_routes`
is nonempty. An example setting is:

```json
{
  "library_routes": {
    "games": {
      "service": "example.independent-library/2",
      "callers": ["example.consumer"],
      "executors": {"runner": "example.library.provider/1"}
    }
  }
}
```

The operator must approve the corresponding registration and call grants in
the selected package and installed policy. This setting does not grant access.
The library's caller policy permits `amaterasu.launch` to `resolve_launch` on
behalf of the permitted consumer. Launch and that consumer both need call grants
for the selected library, executor and session services because admission is
parent-linked. Later unparented monitoring/cleanup runs as the Launch component;
the executor must authorize that initiator too. The development packer's existing
external components and explicit `--call-grants` mechanism can express those
routes. No default grant to an arbitrary new library or executor is introduced.

The consumer submits:

```json
{"action":"launch","operation_id":"consumer-owned-stable-operation","application":{"library":"games","id":"opaque-library-id"},"revision":"entry-v7"}
```

Optional startup-owned preparation can be selected by the logical library and
opaque application ID. For example, inside the `games` route above:

```json
{
  "require_application_policy": true,
  "application_policies": {
    "opaque-library-id": {
      "revision": "entry-v7",
      "working_directory": "/srv/games/example",
      "exit_timeout_seconds": 10,
      "preparation": [
        {
          "run": {"executable": "/usr/local/bin/before-game"},
          "undo": {"executable": "/usr/local/bin/after-game"}
        }
      ]
    }
  }
}
```

These are trusted operator-supplied commands, not library response fields. The
settings are validated at startup, bounded to 256 applications per route and
eight structured preparation steps per application. If policy is present its
revision must match the requested library entry; otherwise launch fails before
session or preparation effects. `require_application_policy` also rejects an
application without an explicit policy. Without this flag, an unlisted entry
retains the original empty-plan behavior. An explicit empty plan is allowed.
Preparation runs before the executor starts; confirmed provider retirement
precedes preparation cleanup and session lease release. Failure or cancellation
uses the existing application preparation lease and cleanup reconciliation. The
policy has no raw or elevated command form, and settings cannot be changed live.
This static policy does not implement global or client-specific preparation,
and it does not add NVHTTP preflight for `/2`.

A route can instead specify `"policy_service":"example.policy/1"` and omit
`application_policies`. Launch requests a private snapshot from that exact
startup-selected service before creating a session. The SDK's
`application_library::policy` defines the versioned request, bounded snapshot,
provider trait and caller/initiator-checking adapter. The resolver echoes the
logical application and opaque entry revision and supplies a separate opaque
`policy_revision`, working directory, exit timeout and structured preparation.
Launch validates the identity and command plan and retains that policy revision
with its owned launch. A missing, rejected, malformed or mismatched response
fails before session or executor effects; there is no static fallback. The
selected consumer and Launch each need the exact resolver call grant for a
parented request, and the provider must independently authorize the broker
caller and initiator. These are test/operator composition choices, not new
default or installed grants. This `/2` route still does not offer streaming
preflight, display admission, or a client-authenticated policy context.

The provider example has opt-in preparation tests using the real runtime
broker, the Launch module, a separately built Application module, and two
library implementations. The first library may also be separately built:

```sh
cargo build --offline --locked -p amaterasu-application
CARGO_TARGET_DIR=/tmp/amaterasu-library-v2 cargo build --offline \
  --manifest-path crates/module-sdk/examples/application-library-v2/Cargo.toml
AMATERASU_APPLICATION_ARTIFACT="$PWD/target/debug/libamaterasu_application.so" \
AMATERASU_LIBRARY_V2_ARTIFACT=/tmp/amaterasu-library-v2/debug/libexample_application_library_v2.so \
  cargo test --offline --locked \
  --manifest-path crates/module-sdk/examples/application-library-provider/Cargo.toml \
  real_preparation_ -- --ignored
```

The example library's baseline lock file may need an offline dependency update
for its standalone build; the command does not require a network. The test
runs harmless test subprocesses and checks reverse undo after failure and
cancellation. Its session and executor remain synthetic. It does not exercise
NVHTTP, RTSP, a real game, or a complete separately built replacement executor.

The logical library key is fixed in startup settings. Launch asks that library
to resolve the exact ID/revision, validates the echoed identity, then maps the
executor key through that library's fixed routes. Neither request nor response
can inject an executable or service. Different libraries/aliases resolving to
the same executor service and executor application ID share an execution lease.

This bundled composition then uses the existing provider lifecycle contract:
pin incarnation, dispatch once, reconcile uncertain starts, and require a
matching fenced-stop assertion before releasing ownership. Lookup, status and
stop remain available through `/2`; retries of an operation must keep the same
library reference and revision. Reusing an operation with changed arguments
returns `operation_conflict`. Replies use the existing Launch envelope
`{ok:true,launch:{...}}` or `{ok:false,code:...}`; a failed owned workflow includes
its state and failure in `launch`. `library_selection` identifies its public
reference/revision; the internal service route is omitted.

`/2` currently accepts direct component consumers only (caller equals initiator).
It does not offer delegated device/user launch context or the legacy streaming
preflight/display-settings contract. Its `describe` response reports version 2,
reference launch, direct callers only, and `durable_recovery:false`. Launch's
operation records remain in memory; this increment does not provide restart
recovery, multiseat, shared lobbies, a new network protocol or packet delivery.
An alternate execution/session plugin can implement a different composition.

## Evidence and limits

The independent [snapshot library example](../crates/module-sdk/examples/application-library-v2/README.md)
builds as its own cdylib. It owns its snapshot, scoped continuation records and
opaque execution references. A second generated-library fixture exercises the
same consumer and Launch implementation with a different library implementation.
Both run through the real runtime broker without registering Catalog, NVHTTP or
transport services. The executor and session effects are synthetic in these
composition tests.

Validation covers separate versions, safe metadata, multipage reads, changed
query/scope/revision, forged fields, route injection, conflicting retries,
unauthorized stop and lost-start cleanup. The opt-in artifact test loads the
separately built snapshot cdylib through its C ABI and browses, launches and
stops through the real broker and Launch module. This proves a library module
boundary, not signed installation, actual game effects, Moonlight compatibility,
hardware behavior or independent packaging of every subsystem.

The [binary transfer API](binary-transfer-api.md) supplies the optional native
bulk-data foundation for artwork. Its independent fixture is qualified; the
Steam/Lutris, facade, HTTP and Moonlight consumers still need migration with the
same provider/revision/authority checks. The legacy chunk contract remains active.

Remaining ecosystem work stays tracked: generic artwork, revision-checked edits
and source/user ownership, authenticated user/device/seat policy, optional source
management and asynchronous actions, and durable execution recovery. This first
increment supplies a usable read/handoff boundary without declaring those
features implemented.

### Selected streaming integration under qualification

The working-tree selected streaming path is not ready for release. It carries a
source-qualified `PolicyIdentity` without converting opaque revisions to numeric
catalog revisions. A configured selected-library profile uses startup-owned,
certificate-scoped Moonlight aliases and a separately selected `/2` streaming
policy authority. The trusted NVHTTP coordinator supplies the paired certificate
fingerprint; the authority must explicitly authorize it. Launch re-resolves the
library target and exact policy before accepting the immutable ordinary-policy
snapshot, then binds display admission and RTSP verification to the identity,
certificate and operation. Legacy catalog operation remains selected when the
startup alias list is empty. A nonempty selected profile replaces the catalog list;
its artwork bridge is not implemented.

This is source work in progress, not evidence of a complete production streaming
composition. Pairing and host-configuration revocation require the separate
live authority fence/drain adapter and must not be replaced by the ordinary
snapshot. The selected policy freezes global and application preparation at
admission, including per-application exclusion of global steps. For an enabled
limiter, the selected executor must advertise the versioned target-attestation
capability. It reserves an executable target for the exact operation, provider
incarnation and tagged policy, then confirms the same target against the running
execution ticket before Launch marks the application active. At stream
activation, Launch rechecks the same running ticket and target with the executor
before RTSP proceeds. The recheck is coordinator-self-initiated, so the selected
executor must allow both the authenticated transport initiator and the Launch
coordinator for this contract. Executors that do not implement the capability
fail closed; native provider assertions remain trusted component assertions,
not a sandbox.

Selected limiter admission also requires the executor to advertise reservation
retirement. Cleanup must confirm an idempotent, owner- and incarnation-bound
retirement fence even if reservation completion is uncertain. A provider keeps
bounded replay fences for its incarnation and rejects new work if full; a late
reserve cannot recreate a retired reservation. Cancelling an admitted preflight
uses the same cleanup path before NVHTTP releases its history reservation.
An unknown preflight after a Launch restart is not a retirement proof: NVHTTP
retains its cleanup obligation until the provider fence can be confirmed. The
in-memory Launch record cannot currently reconstruct that fence after restart,
so such cleanup remains failed and requires a recovery mechanism.

A Linux composition test exercises the real NVHTTP router, Launch, RTSP, pairing
and lifecycle entrypoints with independently built library, policy and executor
implementations and synthetic hardware. It covers attested limiter replacement,
stale and mismatched targets, foreign-owner tokens, stale policy, display
rejection, cancellation and preparation failure. It injects transport trust and
does not test live pairing or host-configuration revocation; those
security-fence cases remain required before integration. Resume retains the
accepted library identity and ordinary-policy snapshot, checks the current
paired certificate and lifecycle owner, and obtains a fresh display admission
for the new RTSP operation. The composition test covers changed library and
policy responses, a supported limiter, stale and foreign lifecycle targets,
unpairing, and failed stream preparation. It stages the NVHTTP paused record
after exercising real lifecycle pause and RTSP close; it does not exercise an
encrypted RTSP PLAY/disconnect or the separate live revocation barrier.

No installed grants or live settings are changed by this work.
