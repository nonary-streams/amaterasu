# Application catalog module

`amaterasu-catalog` builds an independently loadable native `cdylib` and an `rlib`.
It registers `amaterasu.catalog/1`. Required grants: register that service and call
`amaterasu.state/1`; consumers need call grants for `amaterasu.catalog/1` and `amaterasu.state/1`,
because delegated state requests preserve the initiating caller's authority.
Dependencies are the module SDK, serde and serde_json. Native modules are trusted
code; API grants are not a native-code sandbox.

Ordinary operations store a separate manual catalog for each authenticated immediate
caller from runtime `Poll`. Payloads cannot override this identity. Collection names are
opaque `principal/<caller>` strings in the catalog module's state namespace.
All state requests are asynchronous, linked to the original parent request and
use its remaining deadline, capped at 30 seconds. A successful mutation is
returned only after state persistence succeeds. A serial pipeline and state
collection compare-and-set prevent overwriting concurrent durable changes; state
conflicts are returned, never silently retried. Up to 32 requests can wait behind
one active request. Restart reloads data from state rather than an in-memory cache.

## JSON API

Unknown fields are rejected at every nesting level. Nullable fields may be absent
or null. Defaulted arrays and maps may be absent but do not accept null. Read an
empty catalog to obtain revision 0:

```json
{"action":"list","limit":20}
```

Create a manual entry:

```json
{"action":"create","expected_revision":0,"entry":{"name":"Example Game","description":"Local game","tags":["arcade"],"launch":{"kind":"executable","program":"/opt/games/example","arguments":["--fullscreen"],"working_directory":"/opt/games"}}}
```

Executable entries can include ordered preparation and paired undo commands:

```json
{"action":"create","expected_revision":0,"entry":{"name":"Prepared Game","launch":{"kind":"executable","program":"/opt/games/example","preparation":[{"run":{"executable":"/opt/games/configure","args":["enable"],"environment":{"PROFILE":"game"},"cwd":"/opt/games","timeout_ms":10000},"undo":{"executable":"/opt/games/configure","args":["disable"],"environment":{"PROFILE":"game"},"cwd":"/opt/games","timeout_ms":10000}}]}}}
```

The catalog entry is the canonical source of this lifecycle metadata. Preparation
cannot be supplied separately by a launch request. The catalog stores and returns
the commands but never executes them. The application lifecycle consumer runs
`run` commands in order and owes each successfully spawned command's optional
`undo`, including a run that may have had side effects before failing. Owed undo
commands run in reverse order after the application process tree settles.

`preparation` defaults to no steps and is omitted from canonical serialization
when empty. Within a command, absent `args`, `environment`, `cwd`, and `timeout_ms`
canonicalize to `[]`, `{}`, `null`, and `10000`; an absent `undo` canonicalizes to
`null`. Paths and argument strings are retained literally and are not filesystem-
canonicalized or shell-parsed. Create and update responses contain the same
normalized metadata committed to state, and ordinary reads, launch resolution,
restart reloads, and legacy migration preserve it.

Success includes `ok`, catalog `revision`, generated `id` (initially
`app-0000000000000001`) and `entry`. IDs are opaque, caller-local, monotonically
allocated and retained across rename, restart and metadata edits. Deleted IDs are
never reused. They are not portable provider IDs or globally unique UUIDs.

```json
{"action":"get","id":"app-0000000000000001"}
{"action":"update","id":"app-0000000000000001","expected_revision":1,"entry":{"name":"Renamed Game","launch":{"kind":"application","path":"/Applications/Game.app"}}}
{"action":"delete","id":"app-0000000000000001","expected_revision":2}
{"action":"search","query":"arcade","limit":10}
```

`update` replaces all entry metadata; description and tags default to empty.
Arguments default to empty; working_directory defaults to null. All three
mutations require `expected_revision`, referring to the **whole caller catalog**.
Each successful mutation advances it once. Missing IDs return `not_found`.
Delete returns `ok`, `revision`, `id`, `deleted:true`.

List and search return `ok`, `revision`, `entries:[{"id":...,"entry":...}]`, and
`next_cursor` (null when finished). Pass the entire returned cursor as `cursor`
on the next request, preserving the search query. Cursors contain revision,
last ID and normalized query; changes to the catalog invalidate them. Results
are ordered by stable ID. Search performs Unicode lowercase substring matching
on names, descriptions and tags; no fuzzy matching, ranking or Unicode
normalization. Search requires 1–160 printable UTF-8 bytes. Page size defaults to
20 and must be 1–32. Encoded result rows are also capped at 12,000 bytes, so a
page can contain fewer entries than requested. Search examines at most 256
entries per request and may return an empty page with a non-null continuation;
keep following cursors until null. A cursor records the last examined ID.

Errors have shape `{"ok":false,"error":{"code":"...","message":"..."}}`.
Catalog `revision_conflict` includes `current_revision`. State errors are forwarded;
a state-level conflict's revision refers to the state collection rather than the
catalog, so reload the catalog before retrying. Other codes include
`invalid_request`, `invalid_entry`, `invalid_cursor`, `not_found`,
`resource_exhausted`, `invalid_state_response`, and runtime deadline/cancellation
or persistence failures. Mutation retries after an ambiguous timeout are not
idempotent; reload first and reconcile.

## Validation and limits

Entry names are nonblank and at most 160 UTF-8 bytes; descriptions 1024 bytes;
at most 16 nonblank tags of 64 bytes each. Strings reject control characters.
Executable and application paths must be absolute POSIX, Windows drive-rooted,
or UNC paths, at most 2048 bytes, without `.`/`..` path segments. At most 32
arguments of 1024 bytes each are allowed. Validation is structural across host
platforms: no filesystem existence, executable type, bundle validity, network
access, environment expansion, shell parsing or launch authorization is implied.
Arguments remain literal metadata. This module never launches applications.

Preparation is available only for executable entries. At most eight steps are
allowed. Each step requires one `run` command and may contain one `undo` command.
Each command has these limits:

- `executable` and optional `cwd` are native absolute paths of 1–1,024 bytes
  without NUL. `.bat` and `.cmd` executables are rejected case-insensitively;
  choose an interpreter explicitly when needed.
- `args` contains at most 64 literal values, each at most 1,024 bytes and without
  NUL, with at most 8,192 bytes combined.
- `environment` contains at most 32 explicit keys. Keys are 1–128 ASCII
  alphanumeric/underscore bytes and unique ignoring ASCII case; values are at
  most 1,024 bytes without NUL; keys and values total at most 4,096 bytes. The
  lifecycle consumer clears its inherited environment before applying this map.
- `timeout_ms` is 1–30,000 milliseconds and defaults to 10,000.

These commands are trusted native-code launch metadata, not a sandbox or a shell
script interface. Their encoded bytes also count toward the entry-size limit.

Each entry must fit **11,950 serialized bytes** (including escaped strings).
There is no 12 KB whole-catalog limit. Up to **100,000 entries per caller** are
supported, subject to the state's 16 MiB per-leaf bound. Automated tests exercise
10,000 manual entries, bounded transport and pagination. State uses an ordered range index over immutable
copy-on-write leaf shards and an atomically replaced manifest. Each mutation writes
only affected shards, then publishes metadata, entry edits and revision together.
The state root lock serializes writers; collection CAS rejects stale mutations.
Failed or interrupted writes before manifest publication leave the old catalog
visible. Unreferenced shards are ignored and reclaimed after the next successful
commit. A write whose acknowledgement is lost can still have committed.

Existing `principal/<caller>` JSON documents migrate lazily on ordinary access.
The read-only export described below reads legacy documents without migrating them.
Migration publishes every entry with the original IDs, next-ID counter and catalog
revision in one state transaction. The legacy value remains as a recovery copy;
once a collection exists it is authoritative. No payload can select another caller.

List pages and searches use bounded state pages. State follows an ordered range
index and reads only the leaf shards needed to fill each page. Leaves split at
256 entries or a 256 KiB target; paging holds one leaf and a bounded result page
in memory. Substring search still scans entries in stable-ID order; provider
reconciliation and full-text indexing remain separate work. No catalog operation loads the full collection into a
transport message or rewrites the whole catalog for an individual edit.

## Behavioral reference and remaining parity

Vibeshine's catalog-policy interface and macOS application defaults were used
only as behavioral references for durable identity and manual launch metadata.
No source was copied or translated. This implementation does not yet provide
provider discovery/import (Steam, Playnite, Lutris), legacy IDs/aliases, portable
UUIDs, artwork loading/fingerprints, provider identifiers, per-application config
overrides, desktop entries, shell-command launch forms, environment overlays,
launch/session integration or migration from Vibeshine data. These remain explicit
parity requirements. There is no cross-caller aggregation API.

## Launch delegation

`resolve_launch {id}` is a read-only lookup reserved for startup
`launch_principals` (empty by default). The packaged launch provider is explicitly
listed in development policy. This lookup selects the catalog of the runtime-bound
request initiator; the payload cannot name a catalog owner. Ordinary get/list/CRUD
remain scoped to the immediate caller. A launch provider must preserve its incoming
request as the parent of the catalog call. This lets the management API launch its
own library entries without granting general cross-catalog access.

`resolve_exported_launch {id}` is the corresponding public-launch lookup. It
requires the immediate caller to be in `launch_principals` and maps the immutable
runtime request initiator through `export_principals`. The selected namespace is
the mapped startup owner; the request cannot supply an owner, initiator, launch
path, or arguments. Success adds `catalog_owner` to the full entry response so
the launch provider can bind its idempotency and exclusive lease to the canonical
owner. This field is for the trusted launch provider and is not part of the
sanitized public application list. Missing either authority returns
`permission_denied` before state access. This lookup is read-only and, like
`export_list`, reads a legacy document without migrating it.

## Read-only streaming export

Startup `export_principals` maps each authorized consumer to one fixed catalog
owner. It defaults to `{}`. Both sides must be 1–128 ASCII letters, digits, dots,
underscores or hyphens; at most 16 mappings are accepted. Example settings:

```json
{"export_principals":{"amaterasu.nvhttp":"amaterasu.management"}}
```

```json
{"action":"export_list","limit":32,"cursor":null}
```

The authenticated **immediate caller** selects this mapping, independently of the
request initiator. An unmapped caller receives `permission_denied` before state
access. The payload cannot supply an owner, initiator or search query. Call grants
for catalog and delegated state access are still required. The mapping grants
the sanitized list operation and, only when combined with an authorized launch
provider, exported launch resolution. Ordinary CRUD remains caller-scoped.

Success is `{"ok":true,"revision":7,"entries":[{"id":"app-0000000000000001","name":"Example Game"}],"next_cursor":null}`.
Rows contain exactly `id` and `name`; launch paths, arguments, descriptions and
tags are never exported. IDs remain opaque catalog strings; a wire adapter owns
any numeric mapping and must reject values outside its supported range.

Export uses the same ordered, bounded pagination and collection revision checks
as list: limit 1–32 (default 20), rows at most 12,000 encoded bytes, and cursors
`{"revision":7,"after":"app-0000000000000001","query":""}`. A non-null cursor's
`after` is the last returned ID. Follow it until null; catalog changes invalidate
it with `revision_conflict`, and a nonempty cursor query is invalid. No catalog
snapshot is retained across requests. Consumers needing a complete consistent
export must discard accumulated rows on a revision or state failure. Empty
catalogs return revision 0, no entries and a null cursor. This action never writes
or migrates storage, including when reading a legacy document.
