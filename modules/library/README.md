# Library facade

The library forwards client-facing catalog list and resolve requests. Launch
remains owned by `amaterasu.launch/1`.

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
