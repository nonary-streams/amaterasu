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
