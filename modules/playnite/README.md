# Playnite provider

The provider publishes only complete, validated extension snapshots to the
fixed `amaterasu.management` catalog owner. Product settings are versioned and
persisted by `amaterasu.configuration`; older documents without
`autosync_remove_uninstalled` load with its host default of `true`, and a
settings update that omits the optional field preserves its stored value.

`auto_sync` controls membership reconciliation. When it is enabled together with
`autosync_remove_uninstalled`, the provider sends tombstones only for explicit
Playnite records whose `installed` value is `false`. The catalog applies those
tombstones only after a complete snapshot and removes only unchanged entries
still owned by this provider. A manual refresh with `auto_sync` disabled does
not send uninstalled tombstones. Missing IDs, excluded games, and ambiguous
installation state are not treated as uninstalled. The catalog keeps user FEC
overrides on retained entries and does not remove manual or other-provider
entries.

`autosync_delete_after_days` defaults to 14. When `auto_sync` is enabled, a
complete catalog inventory lets the provider remove attached Playnite entries
whose trusted first-import timestamp has reached that age, unless Playnite
reports a play at or after import. A zero value disables age-based cleanup.
Older catalog markers without a trustworthy import timestamp are retained.
`autosync_require_replacement` defaults to true: in recent-game mode, the
provider removes the same number of attached, unselected entries when new
selected games take their places. When false, unselected recent entries remain.
Selection changes and age cleanup are disabled while `auto_sync` is false.
Inventory pages are bound to one catalog collection revision; if it changes
during enumeration, synchronization restarts before writing a snapshot. Cleanup
is committed only after the provider completes that snapshot, and catalog CAS
plus unchanged-entry checks preserve manual edits.

The typed GUID launch route uses the bounded extension connection and ticket
protocol, but full reference focus and fullscreen launch behavior and the
remaining extension synchronization features are still pending. Playnite
selection and age settings have Amaterasu-specific bounds, so this does not
claim every reference input edge is identical.
