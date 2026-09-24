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
catalog unchanged. Lutris is resolved from safe absolute `PATH` directories;
entries launch the client with `lutris:rungameid/<id>` and use detached process
semantics. The URI form preserves Lutris runner-specific launch behavior.

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
positive decimal `id`, a printable `name`, or both. A supplied ID takes
precedence; names match exact case-insensitively. The list is limited to 256
unique entries and 12,000 encoded bytes.

When `include_steam` is enabled, Lutris asks `amaterasu.steam/1` for a complete,
revision-stable paginated game list. It will not write a snapshot if the list is
unavailable, incomplete, contradictory, or over its bounds. A Lutris Steam
entry whose app ID is already supplied directly by Steam is omitted, so the
direct Steam catalog record wins. The catalog import owner is fixed by trusted
startup policy as `amaterasu.lutris` to `amaterasu.management`; request payloads
cannot select or impersonate an owner.

`autosync_remove_uninstalled=true` removes stale Lutris-owned entries only
after a successful complete scan. The catalog preserves manually edited or
detached entries. When removal is false, source IDs absent from the scan are
retained unless an exclusion names them. Name exclusions use a revision-bound
inventory of attached Lutris-owned rows before snapshot begin, so an absent row
can be removed by its previous catalog name without touching another provider's
or a user's entries. Detached rows do not expose a name and remain unchanged.
Currently discovered games that became excluded, non-importable under the Steam
setting, or shadowed by a direct Steam entry are selectively removed after the
complete scan. Catalog revision changes during inventory restart the inventory
before cleanup is submitted.

Other remaining gaps are trusted per-session discovery for machine services,
cross-user import, launch handling for games whose Lutris client cannot be
resolved, and immediate removal of already imported entries when the provider is
disabled.
Behavior was checked against the pinned Vibeshine Lutris policy and database
contract; source code was not copied or translated.
