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

### Durable collection drafts

A State caller may prepare a collection commit in bounded chunks without making
its changes visible to collection reads:

```json
{"action":"collection_draft_begin","collection":"apps","draft_id":"scan_42","expected_revision":7}
{"action":"collection_draft_append","collection":"apps","draft_id":"scan_42","sequence":0,"edits":[{"key":"app-1","value":{"name":"Game"}}]}
{"action":"collection_draft_publish","collection":"apps","draft_id":"scan_42","expected_revision":7,"metadata":{"revision":8}}
```

Drafts belong to the authenticated State caller and the named collection. IDs
contain 1–64 ASCII letters, digits, hyphens or underscores and must be unique
among that caller's active drafts. Begin pins the collection revision and returns
`next_sequence`; repeating begin with the same ID, collection and revision resumes
it. An optional `content_id` on begin binds retries to a caller-declared content
identity: it must be 64 lowercase hexadecimal characters and must match exactly
on resume, including whether it was omitted. State does not compute this digest
or verify it against the edits; callers must derive it from their complete
intended publication. Append sequences begin at zero. Retrying the most recent append with exactly
the same edits is idempotent; different content or an out-of-order sequence is
rejected. Keys must be unique across the entire draft.

Publish checks that the pinned revision is still current, validates all edits,
and atomically replaces the collection manifest once. A revision conflict leaves
both the published collection and the draft intact. After a successful publish,
a retry with the same ID, base revision and metadata returns the published
revision while that publication remains the latest collection commit. A later
commit supersedes this receipt. Call `collection_draft_abort` with `collection`
and `draft_id` to discard a draft; abort is idempotent. Callers should abort
abandoned or conflicted drafts. Drafts persist across restarts. Starting a new
draft reclaims valid drafts idle for at least 24 hours; resuming a draft or
successfully appending refreshes its idle time. Incomplete draft directories from
interrupted creation are reclaimed on the same schedule. Malformed entries and
uncertain timestamps are retained rather than deleted. State serializes its
requests under its root lock, so reclamation cannot race an in-flight operation.

A caller can hold at most 16 active drafts. Each draft is limited to 1,024
chunks, 100,000 edits and 16 MiB of encoded chunk data. Each chunk contains at
most 128 edits and its encoded edits array must fit 12,000 bytes; individual
collection entry and transport limits also apply. These bounds do not guarantee
that every maximum-size catalog can be published in one draft. Publication
materializes the bounded draft in memory and may take longer for larger changes;
a timeout can leave its outcome ambiguous, so retry the same publish request.
