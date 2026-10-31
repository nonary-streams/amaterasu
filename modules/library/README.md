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

The built-in client artwork broker serves artwork only for an exported catalog
entry. A caller sends `artwork_read` with its canonical catalog `id`, a unique
32-character lowercase hexadecimal `transfer_id`, and an `offset` aligned to
8 KiB. The first request uses offset zero and may omit `revision`; the response
supplies the revision to include on subsequent requests. Successful responses
contain base64 `data`, `mime_type`, `total_bytes`, `offset`, and `next_offset`
(`null` for the final chunk). An image is limited to 16 MiB; accepted MIME types
are PNG, JPEG, and WebP. The caller must retain the same transfer ID, revision,
and catalog ID for the whole image, and verify contiguous chunks and consistent
size and MIME type.

After the final chunk or cancellation, the caller sends `artwork_release` with
the same catalog `id` and `transfer_id`. The broker binds each transfer to its
initiator and exported reference, resolves that reference again on each read,
and fences release against in-flight work. A failed or unknown release is not
proof of cleanup; Playnite cleanup can continue asynchronously after the caller
goes away. Transfers and cleanup state are in memory, and provider idle expiry
is a fallback after a module restart, not a durable cleanup acknowledgment.

The catalog-backed broker currently supports Steam, Lutris, and Playnite
artwork. The SDK discovery artwork reference is a separate optional provider
capability: advertising one does not import it into the catalog or make it
available through this broker. Neither interface exposes provider file paths.
