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

The version 1 product document also contains `auto_sync` (`false`),
`exclude_games` (`[]`), `include_tools` (`false`), `sync_all_installed`
(`false`), `recent_games` (`10`), and `recent_max_age_days` (`30`). The provider
`autosync_remove_uninstalled` defaults to `true` and accepts settings updates
with `{enabled,auto_sync,exclude_games,include_tools,
sync_all_installed,recent_games,recent_max_age_days,
autosync_remove_uninstalled,expected_revision}`. The
new fields default when reading older version 1 documents. Older management
updates that omit them preserve their current values. `recent_games` is bounded
to 10000 and `recent_max_age_days` to 36500; either can be zero, with zero games
selecting none and zero days disabling the age limit. The management HTTP
projection/editor exposes these settings; updates may omit the new
field to preserve its persisted value.

Exclusions are bounded to 256 records of the form
`{"id":"730","name":"Counter-Strike 2"}`; either a canonical decimal Steam
app ID or printable name is required. An exclusion matches when its supplied
ID equals the app ID or its supplied name matches the current game name exactly
with ASCII case-insensitive comparison. When both are supplied, either match
excludes the game.

With `auto_sync` enabled, Steam starts discovery when settings are applied.
After each completed scan and its catalog attempt finish, it schedules the next
automatic scan 30 seconds later. It does not start an automatic scan while a
catalog snapshot transaction is active. Catalog reconciliation runs for the
first complete snapshot, when the deterministic fingerprint of represented
Steam source/output fields changes, or as a five-minute artwork retry. The
fingerprint includes every currently represented game field, the selected
artwork revision and source path, and the Steam client executable path. A
settings change clears the successful fingerprint so the new policy is applied
even when discovery is unchanged. Failed or incomplete attempts do not cache a
fingerprint; a new automatic scan is scheduled 30 seconds after the failure.

With `sync_all_installed=false`, the provider selects installed games with
nonzero local `LastPlayed`, filters future timestamps and entries older than the
configured age when `recent_max_age_days` is nonzero, orders newest first (App ID
breaks ties), and takes at most `recent_games`. A zero age disables both age
checks, including the future timestamp check. With
`sync_all_installed=true`, every installed game is selected. Exclusions and
tool filtering apply before recent-game ordering and limits. `include_tools=true`
includes every installed app; the default filters Steam types `tool`, `config`,
`dlc`, `driver`, `music`, and `video`, plus known runtime IDs and runtime-name
fallbacks. Type and name checks lowercase ASCII letters only; non-ASCII UTF-8
characters retain their bytewise identity in runtime-name matching.

App types are read from the configured Steam install's binary
`appcache/appinfo.vdf`; play history is read from every numeric account's
`userdata/<account>/config/localconfig.vdf`, taking the newest timestamp across
accounts. These files are parsed locally and need no Steam Web API key. If the
active selection policy needs app types or recent history and those sources are
missing, malformed, oversized, or incomplete, the catalog is left unchanged.
Configure the Steam installation root (or its `steamapps` directory) as well
as library roots so those sidecars can be found. Discovery and list remain
available when metadata is incomplete. Status includes
`catalog_sources.appinfo` and `catalog_sources.play_history`.

Catalog entries use stable source IDs `steam:<appid>` and launch the resolved
Steam client with `-applaunch <appid>`. Exclusions affect catalog imports only;
the Steam provider's `list` action continues to show all discovered games. The
provider uses the fixed catalog import mapping from `amaterasu.steam` to the
management catalog and accepts no caller-supplied owner selector.

The snapshot protocol writes bounded batches and declares completion only after
a successful full scan. Catalog ownership markers preserve manually changed or
deleted entries. A scan with no configured roots, any discovery warning, or no
resolved Steam client leaves the catalog unchanged. If discovery advances to a
newer revision with the same source fingerprint and settings epoch, the import
can continue. A changed fingerprint or settings epoch cancels stale work before
another step is submitted; a catalog finish already in flight may still
complete, after which a later complete scan reconciles current settings. Status
and list requests continue during catalog retries.

`autosync_remove_uninstalled` defaults to `true`. In `sync_all_installed` mode,
`false` retains older Steam-owned catalog entries that are absent from a
successful scan; a later complete scan with removal enabled can clean them up.
Recent mode always removes entries that fall outside its current selection,
regardless of this setting. Cleanup is fixed to the authenticated importer's own
provider markers, runs only after a complete scan, and preserves detached manual
edits.

This still does not provide complete Vibeshine Steam sync parity. Steam artwork
is local-first: discovery prefers portraits over headers and icons, then picks
the largest bounded image of that class across the configured Steam roots. A
supported local image is served from its source file; otherwise list records
carry a stable opaque reference to the fixed Steam CDN portrait. The authenticated
`POST /v1/steam/artwork_read` action fetches that image on demand and returns the
same bounded chunks as local artwork. The CDN fetch follows no redirects, uses
an eight-second timeout, accepts only JPEG portraits up to 16 MiB, keeps up to
four images in memory, and cools down failed app IDs for ten minutes. The
authenticated chunk route and frontend assembly use the same 16 MiB image
limit. Failed remote fetches leave the UI to use its existing icon fallback.
Vibeshine retains persistent converted covers through transient remote failures
and replaces low-resolution local art when a full portrait is available; those
behaviors remain pending. Amaterasu has no Steam-authorized product cache root
or host service for persistent artwork, so remote images are not persisted or
converted to PNG and a low-resolution local portrait is not replaced remotely.
The sync fingerprint also includes an internal, bounded signature over every
valid local artwork candidate found during discovery, including its canonical
path, byte length, and modification timestamp, plus role and dimensions used for
selection. When auto-sync is enabled, changes to a valid local candidate can
trigger reconciliation even if the selected artwork is unchanged. The signature
is not included in provider or list responses.
Session artwork revisions remain untracked. Playtime remains limited to the
`LastPlayed` values available from supported `localconfig.vdf` sidecars; elapsed
playtime and per-session playtime state are not represented. The Steam
`refresh` action only refreshes discovery; it does not force catalog
reconciliation when `auto_sync` is false, unlike Vibeshine's authenticated
force-sync endpoint. Unlike Vibeshine's legacy host config parser, the product
API rejects negative counts and ages as malformed unsigned values instead of
clamping them to zero.

```json
{"action":"refresh"}
{"action":"status"}
{"action":"list","limit":16}
{"action":"list","revision":1,"after":730,"limit":16}
```

Refresh starts incremental work; repeated refresh while scanning joins the same
scan. The host must keep ticking to finish it. Status returns `scanning`,
`revision`, `game_count`, `warning_count`, bounded `warnings`, `complete`, and
`catalog_sources`, and `visited_entries` (current scan only). `catalog_sources`
reports whether appinfo and play-history metadata were complete for catalog
selection. List returns `games`, `revision`,
`next_after`, `scanning`, and `complete`. Continue with the same revision and
returned `next_after`; null means finished. Page size is 1–32, default 16, and
may be smaller to fit the 16 KiB runtime payload. Stale revisions fail with
`revision_conflict`; continuation without a revision fails with `invalid_cursor`.

Each game includes numeric `app_id`, stable provider ID `steam:<appid>`, name,
library/install directories, raw `state_flags`, `installed`, and `last_updated`.
When available, the record also includes Steam `app_type` and `last_played`.
Installed requires the FullyInstalled flag (4) and an existing installation
directory. A manifest cannot escape `steamapps/common` through dot segments or
path separators in `installdir`. This provider does not resolve per-game
executable paths or user launch options. Duplicate IDs prefer installed copies,
then lexical library path, independent of directory order. Results sort by
numeric App ID.

Completed scans atomically replace the in-memory snapshot, including empty scans
when at least one configured root was successfully scanned. Invalid/unavailable
manifests or libraries produce warnings; `complete:false` means consumers must
not interpret missing entries as uninstallations. No configured roots are also
reported incomplete. During refresh the previous snapshot remains visible. There
is no durable discovery snapshot or cursor epoch across module restarts; clients
must discard old cursors after restart.

Manifest and localconfig parsing supports quoted/unquoted text, braces, line
comments, UTF-8 names, escaped quotes/backslashes and common whitespace escapes.
It rejects duplicates, truncated documents, NUL, directives/includes, excessive
nesting or tokens. The binary appinfo parser reads only Steam's bounded app
records and extracts `common.type`. Manifests are bounded to 1 MiB, localconfig
files to 16 MiB, appinfo caches to 2 GiB (streamed), the appinfo key table to
16 MiB, and an individual appinfo record to 16 MiB. Parsers cap nesting at 16,
string keys at 8192 bytes and values per record at 32768. A scan visits at most
100000 directory entries, retains at most 20000 apps, and reads at most 64 user
accounts per root and 256 localconfig files per scan. Limit exhaustion makes
the relevant metadata source incomplete, so catalog cleanup waits for another
scan. Filesystem operations advance over ticks; a dedicated single worker
handles remote image requests, with one queued request and bounded network
timeout. The worker is joined during module destruction. An individual
filesystem call can still block on unavailable storage.

Behavior reference: sibling Vibeshine `src/steam_integration.h` and
`src/steam_artwork.cpp`; source was not copied or translated. Other missing
parity includes automatic per-session root
discovery, persistent artwork caching and conversion, uninstalled owned games,
app type filtering, play-history selection, launch options/Proton resolution,
launch/process tracking, watchers, and Steam broker environment handling. This
provider neither modifies Steam files.
