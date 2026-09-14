# Local Steam discovery

`amaterasu.steam/1` is a read-only provider, built as a separate Rust library.
It reads explicitly configured Steam installation roots (or `steamapps`
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

Completed scans atomically replace the in-memory snapshot, including empty scans.
Invalid/unavailable manifests or libraries produce warnings; `complete:false`
means consumers must not interpret missing entries as uninstallations. During
refresh the previous snapshot remains visible. There is no durable snapshot or
epoch across module restarts; clients must discard old cursors after restart.

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
copied or translated. Missing parity includes automatic per-session root
discovery, provider/catalog reconciliation, artwork and conversion, uninstalled
owned games, app type filtering, launch options/Proton resolution, launch/process
tracking, watchers, and Steam broker environment handling. This provider neither
launches Steam nor modifies Steam files or the application catalog.
