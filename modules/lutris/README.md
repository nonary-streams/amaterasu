# Lutris provider

`amaterasu.lutris/1` imports installed Lutris games into the shared catalog on
Linux. The provider reads the current process user's `pga.db` from
`$XDG_DATA_HOME/lutris/pga.db`, falling back to
`$HOME/.local/share/lutris/pga.db`. It opens SQLite read-only and selects only
installed rows with a nonempty game configuration path. It does not inspect
other users' home directories. A machine service running as root cannot infer
the active desktop user's identity, so it refuses discovery until a trusted
session scanner exists.

The database is limited to 256 MiB, 10,000 games, 24 MiB of selected row text,
and an eight-second scan. Names and fields are validated against catalog
bounds before any snapshot is started. A missing database, unsupported schema,
malformed row, missing Lutris executable, or incomplete Steam list leaves the
catalog unchanged. Lutris is resolved from safe absolute `PATH` directories.
When `PATH` has no usable client, current-user discovery tries `/usr/bin/lutris`
and `/usr/local/bin/lutris`. A fallback must be root-owned, executable by the
current user, and located beneath root-owned directories that cannot be written
by a group or other users. Two distinct valid fallback clients are ambiguous
and leave the catalog unchanged. Entries launch the canonical client with
`lutris:rungameid/<id>` and use detached process semantics. The URI form
preserves Lutris runner-specific launch behavior. A later scan re-resolves the
client; a client removed after import cannot be revalidated by this provider at
launch time until the shared Launch admission contract supports that check.

Version 1 product settings are owned by Lutris and stored through
`amaterasu.configuration/1` with revision compare-and-swap. Only the management
principal can use `settings_get` and `settings_update`. The response is
`{ok,revision,values}` with `enabled`, `auto_sync`,
`autosync_remove_uninstalled`, `include_steam`, and `exclude_games`. Updates
require `enabled` and `expected_revision`; the other fields are optional patch
values and preserve their current persisted value when omitted. Defaults are
enabled on Linux and disabled elsewhere, automatic sync every 30 seconds,
stale removal enabled, Steam inclusion disabled, and no exclusions.

An exclusion is a positive decimal game ID string or an object containing a
positive decimal `id`, a printable `name`, or both. When both fields are
present, either a matching ID or an ASCII case-insensitive name match excludes
the game. Single-field ID and name exclusions keep the same matching rules. The
list is limited to 256 unique entries and 12,000 encoded bytes.

When `include_steam` is enabled, Lutris asks `amaterasu.catalog/1` for a
revision-paged `export_steam_ids` projection. The request contains no owner
selector; startup policy must grant the separate
`steam_id_export_principals` capability from `amaterasu.lutris` to the fixed
`amaterasu.management` owner that stores direct Steam rows. This capability does
not grant `export_list`. The response contains only numeric Steam IDs, the
catalog revision, and a continuation cursor. Lutris deduplicates repeated
IDs and abandons the scan if a page is malformed, the catalog revision changes,
or the export is unavailable or exceeds its bounds. A child result that remains
unavailable after its request timeout and grace period fails the scan, allowing
the next automatic discovery to retry. Timeout cleanup cancels and releases the
accepted child request in order, with bounded retries if transport fails.
Profiles must grant this catalog export
before the setting can reconcile successfully. A Lutris Steam entry whose app ID
already has a direct catalog row is omitted, so a Steam app
that was discovered but not selected into the direct catalog does not suppress
the Lutris entry. The catalog import owner remains fixed by trusted startup
policy as `amaterasu.lutris` to `amaterasu.management`; request payloads cannot
select or impersonate an owner.

`autosync_remove_uninstalled=true` removes stale Lutris-owned entries only
after a successful complete scan. The catalog preserves manually edited or
detached entries. When removal is false, source IDs absent from the scan are
retained unless an exclusion names them. Name-only and paired ID/name
exclusions use a revision-bound inventory of attached Lutris-owned rows before
snapshot begin, so an absent row can be removed by its previous catalog name.
The inventory is scoped to this provider, so another provider's and a user's
entries are untouched. Detached rows do not expose a name and remain unchanged.
Currently discovered games that became excluded, non-importable under the Steam
setting, or shadowed by a direct Steam entry are selectively removed after the
complete scan. Catalog revision changes during inventory restart the inventory
before cleanup is submitted.

Each enabled scan captures its complete settings at scan start. Disabling Lutris
while discovery, Steam deduplication, or catalog reconciliation is in flight does
not cancel that scan: it completes one snapshot using the captured settings.
Disabled settings prevent later scans from starting, and disabling alone does not
remove already imported catalog rows. Stale removal remains governed by the
captured `autosync_remove_uninstalled` value.

Automatic discovery checks the installed rows every 30 seconds after the
previous scan and catalog work finish. A catalog snapshot is submitted when the
source rows, local image digest, or resolved Lutris executable change, after a settings change, or
at least every five minutes to retry catalog state changes outside Lutris.
Manual refresh always submits a snapshot. A failed scan or reconciliation is
retried on the next automatic discovery; only a validated catalog finish records
the source as reconciled. If settings change during an in-flight scan, that scan
finishes with its captured values and the new settings trigger another scan.
If that old discovery or Steam deduplication fails after disable and re-enable,
the current enabled settings still trigger an immediate automatic scan.
The provider status keeps `refresh_pending` true through catalog completion and
reports a catalog failure in `last_error`; a successful later reconciliation
clears it.
Local artwork revisions are SHA-256 digests of the validated opened image bytes.
An image-only change updates the opaque Catalog reference on the next scan.
Artwork discovery limits accepted image bytes to 256 MiB and checks an
eight-second deadline between image reads; a slow individual read may exceed
that deadline. If the budget is reached, game rows still reconcile and provider
status reports `artwork_scan_incomplete`. Previously known artwork revisions are
retained for games the partial scan did not reach; a complete scan clears images
that are genuinely absent. Readback still rejects a retained revision if the
underlying image changed.

The provider can discover local cover art by safe game slug in Lutris `coverart`
(`png`, `jpg`, `jpeg`, `webp` in that order), falling back to installed hicolor
icons from 256 through 48 pixels. Only regular images within the discovered
user's data tree, up to 16 MiB, are accepted. The signed management principal
can call `artwork_get` with a numeric `game_id` to obtain an opaque revision,
MIME type, and byte count, then `artwork_read` with that ID, revision, and an
8 KiB aligned offset for base64 chunks. Changed images reject stale revisions;
paths are never accepted or returned. Disabling Lutris also disables these
reads and clears the discovered image source. Re-enabling requires a new scan,
even when automatic sync is off; a late scan captured under older settings
cannot publish image sources. Linux reads verify the opened file descriptor's
resolved target under the discovered data tree and use that same descriptor for
metadata and bytes. Other platforms fail closed. Imported rows with readable
artwork include an opaque Catalog Lutris reference; rows without artwork omit it.
Catalog acceptance requires the canonical decimal Lutris source ID artwork
authorization contract. Client image delivery still needs the consuming integration.

Other remaining gaps are trusted per-session discovery and launch for machine
services, cross-user import, Flatpak-only clients, and launch admission for a
retained row after its client disappears or Lutris is disabled.
Behavior was checked against the pinned Vibeshine Lutris policy and database
contract; source code was not copied or translated.
