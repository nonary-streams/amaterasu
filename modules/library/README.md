# Library facade

The library forwards client-facing catalog list and resolve requests. Launch
remains owned by `amaterasu.launch/1`.

## Complete presentation identities

An installed presentation consumer can request
`{"action":"identity_inventory","limit":32,"cursor":null}`. This returns
`{ok,revision,entries,next_cursor}`; entries contain `id`, `uuid`, `title`,
`aliases`, `visible`, `monitor_profile`, and `properties`. IDs and aliases retain
the canonical `app-` plus sixteen lowercase hexadecimal digits. Hidden entries
remain in this inventory so a synthetic control or cached tile cannot reuse
their identities. Ordinary `list` remains a visible `id`/`name` projection.

Each page contains at most 32 ordered entries and fits 12 KiB, so metadata can
produce a shorter page with a continuation. Preserve the complete cursor and
restart after a revision conflict. The SDK supplies request/response types and
whole-page validation. The library removes launch and preparation fields before
publication, validates bounded properties and continuations, and delegates with
the original broker parent. Catalog accepts inventory requests only from its
startup-selected library and configured consumer initiator; payload identities
never select a catalog owner.

Optional provider discovery is disabled by default. At startup, an installed
package may supply module settings such as:

```json
{"discovery":{"providers":{"games":{"service":"example.games/1","allowed_initiators":["example.manager"]}}}}
```

The direct caller sends `{"action":"discover","provider":"games","cursor":null,"limit":20}`.
The logical provider key selects only a service fixed in the startup settings;
requests cannot supply service names. The direct caller must also appear in the
provider's `allowed_initiators`. The library forwards the request with a broker
parent, so the original initiator's service grants and the library's own grants
must both permit the selected service. The adapter must independently authorize
the library caller and original initiator. These grants are not a sandbox for
native modules.

Discovery pages contain at most the requested 1–32 entries and are capped at
12,000 serialized bytes. The provider returns a revision, entries with opaque
IDs, names, and optional `{id,revision}` artwork references, and a next cursor.
The consumer is responsible for checking revision consistency between pages.
Discovery does not import catalog entries or integrate provider launch.

## Exported artwork

`artwork_open {id, revision?}` resolves an exported catalog reference and returns
an owner-bound binary-transfer offer. Providers and both NVHTTP/browser consumers
use the shared SDK. The old JSON/base64 chunk and explicit release protocol has
been replaced; binding release and expiring unclaimed offers bound retention.
See [the artwork contract](../../docs/artwork-transfer.md) for limits, grants,
provider substitution, cleanup and HTTP behavior.
