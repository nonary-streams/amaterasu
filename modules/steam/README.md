# Local Steam discovery

`amaterasu.steam/1` is a separate Rust library. It reads explicitly configured
Steam installation roots (or `steamapps`
directories), follows modern and legacy `libraryfolders.vdf` entries, and parses
`appmanifest_<appid>.acf`. Set startup settings for principal `amaterasu.steam`:

```json
{"roots":["/home/user/.local/share/Steam","/mnt/games/SteamLibrary"]}
```

An absent setting means no roots. Up to 32 configured roots and 64 discovered
libraries are supported. Paths must be native absolute paths. Root symlinks are
canonicalized to support ordinary Linux Steam layouts; manifest symlinks are
rejected. This is trusted local-user discovery, not a filesystem sandbox or a
cross-user session broker. Every caller granted `call` for this service can read
the configured library metadata and initiate refresh. Payloads cannot supply new
filesystem roots. Provider requires `provide` for its service only.

The domain-owned product setting `enabled` controls discovery. It defaults to
false except on Linux, where the Vibeshine policy forces Steam enabled. A
disabled provider rejects refresh and drops its in-memory scan and snapshot;
enabling it allows callers with the Steam service grant to request an explicit
refresh. Settings are stored through `amaterasu.configuration/1` as version 1
product settings with revision-based compare-and-swap. Only the management
principal can use the provider's `settings_get` and `settings_update` actions;
the fixed management routes expose `/v1/steam/settings` and
`/v1/steam/settings_update`. Startup roots remain bootstrap settings and cannot
be changed through these product settings routes. While the initial product
document loads or a write is pending, status and list requests remain available
and include `settings_ready`; refresh waits for initial settings, and overlapping
settings actions return `settings_loading` or `settings_busy`. If an update's
result is ambiguous and the first durable readback fails, management receives an
unknown-outcome error while Steam retries the readback with capped backoff; other
Steam requests continue to be served during those retries.

The product document also contains `auto_sync` (default `false`) and
`exclude_games` (default `[]`). `settings_update` accepts
`{enabled,auto_sync,exclude_games,expected_revision}`. Exclusions are bounded to
256 records of the form `{"id":"730","name":"Counter-Strike 2"}`; either a
canonical decimal Steam app ID or printable name is required. When an ID is
provided, matching uses the ID; a name-only exclusion matches exact names
without case sensitivity. Existing version 1 documents that only contain
`enabled` load with auto-sync off and no exclusions.

With `auto_sync` enabled, Steam starts discovery when settings are applied and
then requests another scan every 30 seconds. It imports a scan only when every
configured library and manifest was read without warnings and a Steam client
executable was resolved. Catalog entries use stable source IDs `steam:<appid>`
and launch the resolved Steam client with `-applaunch <appid>`. Exclusions affect
catalog imports only; the Steam provider's `list` action continues to show all
discovered games. The provider uses the fixed catalog import mapping from
`amaterasu.steam` to the management catalog and accepts no caller-supplied owner
selector.

The snapshot protocol writes bounded batches and declares completion only after
a successful full scan. Catalog ownership markers preserve manually changed or
deleted entries, and stale Steam-managed entries are removed only by a complete
scan. A scan with no configured roots, any discovery warning, or no resolved
Steam client leaves the catalog unchanged. If settings or discovery revision
changes during an import, Steam stops before submitting another step; a catalog
finish already in flight may still complete, after which a later complete scan
reconciles current settings. Status and list requests continue during catalog
retries.

This is installed-manifest reconciliation, not complete Vibeshine Steam sync
parity. The current manifests do not expose local play history, Steam app type,
or artwork, so recent-game selection and tool classification remain pending.
Auto-sync imports installed manifests, and the `steam_autosync_remove_uninstalled`
policy is not configurable yet.

```json
{"action":"refresh"}
{"action":"status"}
{"action":"list","limit":16}
{"action":"list","revision":1,"after":730,"limit":16}
```

Refresh starts incremental work; repeated refresh while scanning joins the same
scan. The host must keep ticking to finish it. Status returns `scanning`,
`revision`, `game_count`, `warning_count`, bounded `warnings`, `complete`, and
`visited_entries` (current scan only). List returns `games`, `revision`,
`next_after`, `scanning`, and `complete`. Continue with the same revision and
returned `next_after`; null means finished. Page size is 1–32, default 16, and
may be smaller to fit the 16 KiB runtime payload. Stale revisions fail with
`revision_conflict`; continuation without a revision fails with `invalid_cursor`.

Each game includes numeric `app_id`, stable provider ID `steam:<appid>`, name,
library/install directories, raw `state_flags`, `installed`, and `last_updated`.
Installed requires the FullyInstalled flag (4) and an existing installation
directory. A manifest cannot escape `steamapps/common` through dot segments or
path separators in `installdir`. This does not classify games versus tools/DLC or
resolve executable paths. Duplicate IDs prefer installed copies, then lexical
library path, independent of directory order. Results sort by numeric App ID.

Completed scans atomically replace the in-memory snapshot, including empty scans
when at least one configured root was successfully scanned. Invalid/unavailable
manifests or libraries produce warnings; `complete:false` means consumers must
not interpret missing entries as uninstallations. No configured roots are also
reported incomplete. During refresh the previous snapshot remains visible. There
is no durable discovery snapshot or cursor epoch across module restarts; clients
must discard old cursors after restart.

The parser supports quoted/unquoted text, braces, line comments, UTF-8 names,
escaped quotes/backslashes and common whitespace escapes. It rejects duplicates,
truncated documents, NUL, directives/includes, excessive nesting or tokens. It
does not parse binary appinfo/userdata or implement the entire KeyValues dialect.
Each file is bounded to 1 MiB, nesting to 16 levels, tokens to 8192 bytes, and
fields to 32768. A scan visits at most 100000 directory entries and retains at
most 20000 apps; limit exhaustion makes the scan incomplete. One library open or
manifest read runs per tick, with no background threads outliving the library.
Individual filesystem operations can still block on unavailable storage.

Behavior reference: sibling Vibeshine `src/steam_integration.h`; source was not
copied or translated. Other missing parity includes automatic per-session root
discovery, artwork and conversion, uninstalled owned games, app type filtering,
play-history selection, launch options/Proton resolution, launch/process
tracking, watchers, and Steam broker environment handling. This provider neither
modifies Steam files.
