# Persistent state service

`amaterasu.state/1` stores JSON values in the actual runtime caller's namespace.
Startup settings require `{"data_directory":"/private/module/data"}`. The module
holds an exclusive directory lock until destruction, including across requests.
Native modules are trusted code; this service is not a native-code sandbox.

Request payloads use the following shapes:

```json
{"action":"get","key":"theme"}
{"action":"set","key":"theme","value":"dark","expected_revision":0}
{"action":"delete","key":"theme","expected_revision":1}
{"action":"snapshot"}
{"action":"snapshot","limit":32,"cursor":{"revision":2,"after":"theme"}}
```

`expected_revision` is optional and compares against the whole caller namespace.
A new namespace has revision zero. Each accepted mutation, including deleting a
missing key or setting an unchanged value, advances it once. Conflicts never
modify storage. Persistence completes before success is returned.

Success payloads contain `ok:true` and `revision`. Get adds `found` and `value`
(null for missing keys); delete adds `deleted`; snapshot adds a `values` object
and `next_cursor`. Snapshot keys are lexically ordered. Pass `next_cursor` back
as `cursor` to continue; null means the traversal is complete. The optional
`limit` accepts 1–128 keys (default 128); the encoded byte bound may produce a
smaller page. Cursor revisions must match current storage, so mutations between
pages produce `revision_conflict` and require restarting the traversal.
Errors contain `{"ok":false,"error":{"code":"...","message":"..."}}`.
`revision_conflict` additionally includes `error.current_revision`.

Keys contain 1–256 UTF-8 bytes without control characters. Values are limited to
16 KiB minus response metadata, including the key and continuation cursor. Set
validates actual JSON encoding, escaping, and worst-case revision width before
persisting, ensuring the value fits both get and snapshot responses. Every
response is capped at 16,384 serialized JSON bytes. Namespaces are limited to
4,096 keys and 1 MiB of persisted
JSON including metadata. Unknown request fields are rejected. Callers cannot
supply a namespace or a filesystem path. Names are hex encoded into bounded path
components. Stored namespace format version 1 contains `format_version`,
`revision`, and `values`. Corrupt or unsupported state fails closed and remains
untouched for operator repair. Storage failures are request domain errors and do
not invalidate module health.

The service processes at most 16 requests per tick. If persistence finishes after
a caller's timeout or cancellation, the expired completion is discarded without
failing module health; the mutation may already be durable. Cross-namespace transactions,
subscriptions, migrations from other applications, automatic corruption repair,
and Vibeshine state parity beyond these operations are not implemented.

## Sharded collections

Collections supplement the existing small JSON key/value namespace. The immediate
caller still owns the namespace; `collection` is an opaque string with the same
1–256 byte restrictions as keys. Collection revisions are independent of the
legacy namespace revision and of other collections.

```json
{"action":"collection_get","collection":"apps","key":"app-0000000000000001"}
{"action":"collection_page","collection":"apps","revision":1,"after":null,"limit":32}
{"action":"collection_commit","collection":"apps","expected_revision":0,"metadata":{"next_id":2},"edits":[{"key":"app-0000000000000001","value":{"name":"Game"}}]}
```

`collection_get` returns `found` (whether a committed collection exists),
`revision`, `metadata`, `count`, and `value` (null if the requested key is absent).
Omit `key` to read only collection metadata. Page results contain ordered
`entries:[{"key":...,"value":...}]` and `next` (last returned key, or null).
Pass `next` back as `after` with the same revision; stale pages are rejected.
Limits are 1–128 rows and 14,000 encoded row bytes, always inside the 16 KiB
runtime envelope. A commit atomically replaces metadata and applies all edits;
null edit values delete keys, and collection values themselves cannot be null.
Duplicate edit keys are rejected. Metadata is at most 1,024 encoded bytes and a
commit has at most 128 edits, also subject to the runtime transport bound.

Collections support 100,000 items with an ordered range index over immutable
leaf shards. Each encoded key/value row is at most 12,000 bytes. Leaves split
at 256 rows or a 256 KiB target, with a hard 16 MiB serialized leaf bound.
Empty leaves are discarded except the first range. The manifest is bounded at
64 MiB even under adversarial insert/delete fragmentation. Mutations
write and sync changed shards before atomically publishing a manifest containing
their generations, count, metadata and new revision. The store's exclusive root
lock ensures one writer; CAS protects callers against stale revisions. Reads
trust only manifest-referenced immutable shards. A failed pre-publication write
leaves the previous commit visible; a failure after publication can have committed
and requires reconciliation. Reclamation runs only after a durable manifest, so
interrupted writes leave harmless orphan shards until a later successful commit.

Ordered paging seeks directly through the manifest range index and reads only
the leaves needed to fill the page plus one lookahead row. It retains one leaf
and a bounded result page, without scanning the entire collection. Manifest
validation checks revision references, range bounds, metadata size and aggregate
counts; leaf loading also validates each range and its recorded count. Tests persist and reopen 10,000 entries, traverse them with
16 KiB envelope assertions, exercise isolation and stale revisions, and inject a
failure between shard persistence and manifest publication.
