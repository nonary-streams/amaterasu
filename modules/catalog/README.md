# Application catalog module

`amaterasu-catalog` builds an independently loadable native `cdylib` and an `rlib`.
It registers `amaterasu.catalog/1`. Required grants: register that service and call
`amaterasu.state/1`; consumers need call grants for `amaterasu.catalog/1` and `amaterasu.state/1`,
because delegated state requests preserve the initiating caller's authority.
Dependencies are the module SDK, serde, serde_json and SHA-256 for provider-owned
entry fingerprints. Native modules are trusted
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

Entries may include sparse application `config_overrides`. The optional
`amd_av1_latency_mode` string accepts exactly `auto`, `none`, `power_saving`,
`realtime`, or `lowest` (case-sensitive). Omit it or set it to null to inherit;
an explicit `auto` remains an application override. Catalog validates, stores,
and returns this token unchanged; it does not resolve global or client policy.
Existing entries omit the field, so no catalog migration is needed.

Entries may also include a top-level `output` command-log destination. Catalog
stores the exact string, including whitespace, `$()` references, and relative
paths; it does not expand or open it. Values are limited to 1,024 UTF-8 bytes
without control characters. Empty text and the literal string `"null"` are
preserved as distinct values and both request discarded command output.
Omission or JSON null canonicalizes to an absent field. Because `update`
replaces the whole entry, either clears a previously saved destination. Existing
entries remain compatible without a document or provider-marker version bump.
The launch owner resolves a saved file destination when it snapshots the entry;
catalog readback and restart preserve the original text.
The log destination covers owned application, preparation, and detached
commands. It does not route output from an externally provider-owned game.

The optional `dd_hdr_request_override` string accepts exactly `auto`,
`force_on`, or `force_off` (case-sensitive). Omit it or set it to null to
inherit; an explicit `auto` remains an application override. Catalog validates,
stores, and returns this token unchanged, and provider refreshes preserve it.
Existing entries and version-1 provider markers may omit it, so no catalog
document or provider-marker version bump is needed. Catalog storage does not
apply this request to a display provider; runtime projection remains outside
this module.

The optional `nvenc_spatial_aq` override is a strict boolean. Omit it or set it
to null to inherit; explicit `true` and `false` values are stored and returned,
including through provider-marker refreshes. Existing entries and markers may
omit this field, so no catalog or marker version bump is needed.

The optional `prefer_10bit_sdr` override is also a strict boolean. Catalog stores
and returns it, including through provider-marker refreshes; absent or null
means inherit, while explicit `true` and `false` remain distinct values.
Existing entries and version-1 provider markers may omit this field, so no
catalog schema or marker version bump is needed. This documents Catalog storage
only: L02 effective application-over-paired-client resolution and runtime
projection remain pending. L03 outbox item 0071 records that there is no current
video/encode field to project this setting to.

The optional `native_pen_touch` strict boolean corresponds to the stable setting
ID `global:native_pen_touch`. Catalog stores only this sparse per-application
override: absent or null means inherit, while explicit `true` and `false` remain
distinct. Catalog does not resolve global or client settings and does not
project this value into launch requests. Existing entries and version-1 provider
markers may omit it, so no catalog document or provider-marker version bump or
migration is required.

```json
{"action":"create","expected_revision":0,"entry":{"name":"AV1 and 10-bit SDR preferences","config_overrides":{"amd_av1_latency_mode":"power_saving","nvenc_spatial_aq":false,"prefer_10bit_sdr":true},"launch":{"kind":"application","path":"/games/example"}}}
```

Executable entries can include ordered preparation and paired undo commands:

```json
{"action":"create","expected_revision":0,"entry":{"name":"Prepared Game","launch":{"kind":"executable","program":"/opt/games/example","preparation":[{"run":{"executable":"/opt/games/configure","args":["enable"],"environment":{"PROFILE":"game"},"cwd":"/opt/games","timeout_ms":10000},"undo":{"executable":"/opt/games/configure","args":["disable"],"environment":{"PROFILE":"game"},"cwd":"/opt/games","timeout_ms":10000}}]}}}
```

The catalog entry is the canonical source of application-specific lifecycle
metadata. The application-specific commands cannot be supplied separately by a
launch request. The catalog stores and returns them but never executes them.
The application lifecycle consumer runs
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

An entry may also carry the raw Vibeshine preparation list at the entry's
top level. Amaterasu stores this list as canonical `prep_cmd`, while accepting
the legacy spelling `prep-cmd` on input:

```json
{"action":"create","expected_revision":0,"entry":{"name":"Legacy prepared game","prep-cmd":[{"do":"  configure --profile=\"two words\"  ","undo":" restore --profile=\"two words\" ","elevated":true}],"launch":{"kind":"executable","program":"/opt/games/example"}}}
```

Each raw item accepts `do` and `undo` strings (both default to empty) and an
`elevated` boolean (default false). Command text is retained byte-for-byte,
including surrounding whitespace. Each string is limited to 2048 UTF-8 bytes
and may not contain NUL; a catalog entry may contain at most eight combined
structured and raw application steps. Raw commands are not shell-parsed or
expanded by catalog. On launch, they are forwarded as raw items alongside
structured preparation steps. Raw steps follow structured steps within each
global or application group, and global steps precede application steps.

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

`update` replaces entry metadata; description and tags default to empty. A trusted
provider artwork reference is retained when the launch target and Steam identity
remain unchanged. Callers cannot supply their own provider artwork reference.
Arguments default to empty; working_directory defaults to null. All three
mutations require `expected_revision`, referring to the **whole caller catalog**.
Each successful mutation advances it once. Missing IDs return `not_found`.
Delete returns `ok`, `revision`, `id`, `deleted:true`.

Executable lifecycle options are sparse per-application values. When omitted,
launch resolves `auto_detach` and `wait_all` to `true` and
`exit_timeout_seconds` to 5 seconds; these defaults are not written into the
catalog document. Typed Playnite launch targets default to a 10-second exit
timeout. `detached` defaults to an empty ordered list. The accepted
aliases `auto-detach`, `wait-all`, and `exit-timeout` are serialized using their
canonical underscore names. `exit_timeout_seconds` is limited to 300;
`detached` accepts up to 16 nonempty command strings of 2048 bytes each.
Detached strings are parsed by the application module at launch time, so catalog
validation checks their bounds and control characters while launch validation
checks command syntax. These fields configure the application process manager;
catalog persistence alone does not establish behavior parity on each platform.

List and search return `ok`, `revision`, `entries:[{"id":...,"entry":...}]`, and
`next_cursor` (null when finished). Pass the entire returned cursor as `cursor`
on the next request, preserving the search query. Cursors contain revision,
last ID and normalized query; changes to the catalog invalidate them. Results
are ordered by stable ID. Search performs Unicode lowercase substring matching
on names, descriptions, tags, launch targets, output, and linked source details;
no fuzzy matching, ranking or Unicode
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
No source was copied or translated. The catalog stores sparse per-application
configuration overrides, including stream fields, the video fields
`max_bitrate`, `minimum_fps_target`, and `video_max_batch_size_kb`, frame limiter
settings, and `ds4_back_as_touchpad_click`. Missing values and null inputs
inherit; explicit false and zero remain present. The launch provider retains
these values in launch and status results. NVHTTP consumer resolution and
management UI support remain cross-module parity requirements. Other catalog
gaps include legacy IDs/aliases, portable UUIDs, artwork loading and
fingerprinting, artwork for the seeded Desktop entry, shell-command launch
forms, environment overlays, and migration from Vibeshine data. There is no cross-caller aggregation
API.

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
owner for the sanitized `export_list` action. It defaults to `{}`. Both sides
must be 1–128 ASCII letters, digits, dots, underscores or hyphens; at most 16
mappings are accepted. Example settings:

```json
{
  "export_principals":{"amaterasu.nvhttp":"amaterasu.management"},
  "steam_id_export_principals":{"amaterasu.lutris":"amaterasu.management"}
}
```

`steam_id_export_principals` is a separate startup map for
`export_steam_ids`. It follows the same identifier validation and size bound as
`export_principals`, but grants no `export_list` authority. Profiles map a
consumer to the catalog owner that contains the direct Steam rows it needs to
check. The action returns only canonical nonzero numeric `steam_id` values, the
catalog revision, and an opaque continuation cursor. It accepts no owner
selector and never returns catalog entry data:

```json
{"action":"export_steam_ids","limit":32,"cursor":null}
```

Success is `{"ok":true,"revision":7,"steam_ids":[730,440],"next_cursor":null}`.
Steam IDs can repeat when separate catalog entries carry the same ID; consumers
that need membership should treat the values as a set. Cursors use the same
revision check and page limit as `export_list`, with the query tag `steam_ids`
and the last scanned catalog key as `after`. Catalog changes invalidate a
cursor with `revision_conflict`.

```json
{"action":"export_list","limit":32,"cursor":null}
```

The authenticated **immediate caller** selects the applicable mapping,
independently of the request initiator. An unmapped caller receives
`permission_denied` before state access. The payload cannot supply an owner,
initiator or search query. Call grants for catalog and delegated state access
are still required. `export_principals` grants only `export_list` and anchors
exported launch resolution when combined with an authorized launch provider;
`steam_id_export_principals` grants only `export_steam_ids`. Ordinary CRUD
remains caller-scoped.

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

## Provider snapshot import

Startup `import_principals` maps each trusted provider caller to one fixed catalog
owner. It defaults to `{}` and has the same 16-entry principal validation as
`export_principals`. Release policy maps `amaterasu.steam`,
`amaterasu.playnite`, and `amaterasu.lutris` to `amaterasu.management`. The importer identity comes from the
authenticated immediate caller of `Poll`; requests do not accept an owner or
domain selector. The catalog stores provider ownership and sync state in separate
reserved collection rows, not in user-editable `Entry` metadata.

Begin a source snapshot and retain the returned generation and catalog revision:

```json
{"action":"provider_snapshot_begin","sync_id":"steam-scan-42","source_revision":42}
```

```json
{"ok":true,"revision":0,"generation":1,"next_sequence":0}
```

Providers may first enumerate their existing ownership markers with
`provider_snapshot_inventory`. The importer is fixed by the authenticated Poll
caller and startup import policy. Request the first page with both `after` and
`expected_collection_revision` null, then pass the returned revision and last
`source_id` on each later page. Providers that need names for attached rows may
opt in with `include_entry_names`:

```json
{"action":"provider_snapshot_inventory","after":null,"expected_collection_revision":null,"limit":32,"include_entry_names":true}
```

The response includes `collection_revision`, `total_count`, sorted marker rows,
and `next_after`. Every page is checked against that collection revision.
Providers should require the final row count to match `total_count` before
beginning cleanup and pass the same revision to `provider_snapshot_begin`;
revision drift returns `revision_conflict`. Marker rows include the stable
`source_id`, nullable trusted `added_at_unix`, provider `source_flags`, and
`attached`, which is false after the entry was manually edited. With
`include_entry_names`, `entry_name` is returned only for an attached entry
owned by that caller; detached or changed entries have a null name. Manual rows
and other providers' rows are never included. Unknown import timestamps remain
null so consumers can fail closed on age-based policies.

Send batches with stable source IDs and validated catalog entries. Each batch
contains 1–8 combined entries and removal intents, at most 12,000 encoded bytes.
Source IDs cannot repeat or overlap within a batch or across batches in one
generation. A batch may carry explicit `remove_source_ids` (uninstalled),
`expire_source_ids` (age policy), and `selection_remove_source_ids` (replacement
or selection policy). An ID may have only one intent in a batch. These
reason-tagged intents are accepted only from the fixed provider importer;
generic catalog management cannot forge them. `expected_revision` is the public
catalog revision returned by the previous batch; `sequence` must equal
`next_sequence`:

```json
{"action":"provider_snapshot_batch","sync_id":"steam-scan-42","generation":1,"source_revision":42,"sequence":0,"expected_revision":0,"entries":[{"source_id":"steam:480","entry":{"name":"Spacewar","launch":{"kind":"application","path":"/Applications/Spacewar.app"}}}]}
```

```json
{"ok":true,"revision":1,"generation":1,"next_sequence":1}
```

An importer may include `remove_source_ids` in a batch to record explicit
provider facts that an owned item is uninstalled. These IDs are tombstones;
they are not inferred from missing IDs or from an entry being filtered out of the
selected catalog. A tombstone only matches a marker owned by the authenticated
importer. Unknown IDs are ignored. Ordinary catalog management actions cannot
write tombstones.

Playnite artwork is an opaque typed reference with a lowercase canonical GUID
and lowercase SHA-256 revision. For artwork-bearing items, the source ID, launch
game ID and artwork game ID must match and the caller must be the authenticated
Playnite importer. When artwork cannot be inspected, that importer can set
`preserve_artwork:true` on the snapshot item to retain matching artwork on an
attached entry. A known absence omits both the reference and that flag, clearing
the previous artwork. Detached entries remain user-owned.

The last committed batch can be retried after an ambiguous timeout. The replay
must have the same sequence, expected revision and SHA-256 payload fingerprint;
it returns the committed revision without applying the batch again. Earlier or
out-of-order sequences return `sequence_conflict`. State collection compare-and-
set also detects edits by ordinary catalog clients; reload/retry after a
`revision_conflict`.

Finish only after the provider has completed its full source scan. `remove_stale`
is an optional boolean that defaults to `true` for existing clients. The separate
`remove_uninstalled`, `remove_expired`, and `remove_selection` flags default to
`false` and enable cleanup for their matching reason-tagged intents:

```json
{"action":"provider_snapshot_finish","sync_id":"playnite-scan-42","generation":1,"source_revision":42,"expected_revision":1,"next_sequence":1,"complete":true,"remove_stale":false,"remove_uninstalled":true,"remove_expired":true,"remove_selection":true}
```

Success returns `ok`, `revision`, `generation`, `complete:true`,
`cleanup_pending:false`, and the resolved cleanup policies. An incomplete scan
is rejected and never removes anything. When `remove_stale` is false, old
provider markers and entries are retained for a later complete scan. Each
cleanup flag applies only to intents with that reason in this generation.
Cleanup removes an entry only if it remains unchanged and attached to this
provider; a detached or edited entry is retained and its marker is cleared.
Other providers' and manual entries are unaffected. All cleanup choices are
persisted with the finished generation, so retries after an ambiguous timeout
must repeat the same choices. Removal is committed in bounded chunks and can be
reconciled by repeating that finish. Begin retries with the
same `sync_id` resume that generation; a new or restarted source scan must use a
fresh ID so partial data from the old generation cannot be mistaken for a
complete snapshot.

### Opt-in staged snapshots

Set `"staged":true` in `provider_snapshot_begin` to durably collect batches
without publishing their entries or removal intents. Retry begin with the same
`sync_id`, source revision, and mode to resume after restart. Batch replies keep
the catalog revision unchanged. A complete finish rescans the collection and
publishes all entry changes, provenance changes, and selected cleanup in one
compare-and-set commit. Manual edits made before that scan remain protected;
concurrent writes during publication cause a revision conflict. The caller must
retry with the current catalog revision. An incomplete finish cannot publish.
Playnite provider epochs still fence revoked generations.

The staged state row, including its key and serialized value, is limited to
10,000 bytes. Publication must fit both 128 collection edits and a 16,384-byte
serialized command. A `resource_exhausted` response leaves the published catalog
unchanged; previously accepted staged batches remain available. These limits make
this mode suitable only for bounded snapshots and cleanup sets. Starting a fresh
snapshot requires a new `sync_id`. Omit `staged` for the existing incremental
batch and cleanup behavior.

Playnite uses explicit `installed:false` records as tombstones only when both
`auto_sync` and `autosync_remove_uninstalled` are enabled. A manual refresh with
`auto_sync` disabled does not apply membership cleanup. Selection exclusions and
missing IDs do not imply uninstallation, and are never tombstoned by the
provider. Playnite age and replacement cleanup use the separate
`expire_source_ids` and `selection_remove_source_ids` intents; imported markers
without trusted timestamps are ineligible for age cleanup.

The catalog API provides durable snapshot mechanics, not source discovery or
product policy. Providers remain responsible for validating their domain data,
constructing `Entry` values, and declaring a scan complete only when every source
page was read successfully.

## Playnite Fullscreen launch entries

Manual entries may use the typed launch value below. It contains no executable
target, URI, or Playnite game ID; the fixed Playnite provider resolves its
installation and owns the correlated launch ticket. `exit_timeout_seconds`
defaults to 10 and is bounded to 0..=300.

```json
{"action":"create","expected_revision":0,"entry":{"name":"Fullscreen","launch":{"kind":"playnite_fullscreen","exit_timeout_seconds":10}}}
```

Both `playnite` and `playnite_fullscreen` launch targets may include an optional
`working_directory` (legacy alias `working-dir`). It must be an absolute path
of at most 1024 bytes without dot segments, matching the downstream launch
protocol's bound. Launch expands configured `$()` references and trims matching
outer quotes, then sends it as the explicit preparation working directory. It
does not select or launch the Playnite executable.

The Playnite provider also reconciles one reserved `Playnite (Fullscreen)` row
through the internal `provider_playnite_fullscreen_entry` action. The authenticated
Poll caller must be the fixed Playnite importer. The generated source ID is
`fullscreen-entry`; no caller-supplied owner or source selector is accepted.
The provider uses this row only when Playnite is enabled, on Windows, and
`fullscreen_entry_enabled` is true. Generic catalog updates and deletes reject
the generated row with `provider_entry_managed`. The action and reserved row
are independent of Playnite library snapshots, so incomplete scans cannot
create or remove them. Snapshot cleanup explicitly excludes this source ID.

Sparse software encoder overrides support exact `sw_preset`/`sw_tune` strings
(up to 128/256 UTF-8 bytes, no controls), `encoder: "auto" | "software"`, and
a requested `min_threads` integer from 1 through 256 (default 2). Null/absence
inherit; empty tuning strings remain explicit. The actual x264 backend validates
codec vocabulary when opening software and may cap the effective thread count
based on frame size or runtime limits. NVHTTP resolves each field client before
application before global and freezes the launch snapshot.
See [software policy](../nvhttp/README.md#software-encoder-policy) for runtime
availability, source differences and remaining qualification gates.

## Desktop launch entries

A manual entry may use the process-less `desktop` launch kind, so a paired client
can start a desktop session with no game:

```json
{"action":"create","expected_revision":0,"entry":{"name":"Desktop","launch":{"kind":"desktop"}}}
```

The target accepts only optional `working_directory` (alias `working-dir`, same
absolute-path bounds as Playnite targets), `exit_timeout_seconds` (default 10,
0..=300, serialized in canonical form) and structured `preparation` with paired
`undo` commands, plus the entry-level `prep_cmd` and `exclude_global_prep_cmd`.
The shared eight-step limit applies. `program`, `path`, `arguments`, `detached`,
`game_id`, `steam_id`, `steam_install_dir`, artwork, `elevated` and any unknown
field are rejected, so the entry cannot name something to run. Provider snapshots
cannot import this kind; it is user-managed. Catalog stores and exports it like
other kinds (`export_list` still returns only `id` and `name`) and never executes
anything. The launch module owns session behavior; see its README.

### First-run seed

The startup setting `seed_desktop_entry` (strict boolean, default `false`) creates
one entry named `Desktop` with launch `{"kind":"desktop","exit_timeout_seconds":10}`.
It applies to each catalog owner named by a value of `export_principals`, the
catalogs served to streaming clients. A hidden row, `~seed.desktop`, is committed
atomically with the entry, so every later write preserves it. Once the marker
exists the seed never runs again: deleting or renaming the entry cannot recreate
it, and a restart reads the marker and writes nothing. Enabling the setting on an
existing catalog adds the entry once. The marker row is skipped by list, search,
export and provider scans and counted with the provider rows.

The seed runs as a startup job between ordinary requests, which wait for it. Its
state requests are parentless and use the catalog's own state grant. A failed
attempt is retried every five seconds up to eight times and then abandoned until
the next start; the marker is still absent, so the next start tries again. A
catalog that still holds an unmigrated legacy document is skipped, because the
first ordinary access migrates it and seeding first would fork it. No Desktop
entry is created without the marker commit, so a lost commit acknowledgement
reconciles by reading the marker. Real Moonlight client acceptance of a Desktop
launch is pending; see the launch README.

### Generic provider-owned launch targets

A snapshot importer may write `{"kind":"provider","provider":"key",
"application_id":"source-id"}` only when startup settings contain both
`import_principals` for the authenticated importer and a unique
`provider_launch_principals` mapping from that importer to `key`. The application
ID must equal the snapshot row's source ID. Provider keys are 1–128 ASCII letters,
digits, dots, underscores or hyphens; source IDs are 1–96 printable bytes.
Optional `exit_timeout_seconds` is 1–300 (default 10), `working_directory` and
`preparation` follow existing target validation. Manual create and update cannot
introduce or replace these targets. At launch resolution, the current mapping and
attached provider marker must still match the target, source ID and entry
fingerprint; otherwise resolution fails closed. Removing or changing a mapping
can therefore make existing entries unavailable. Launch must separately bind the
key to an approved service and verify the provider lifecycle; catalog storage
does not start an application or prove cleanup.
