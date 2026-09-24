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

This provider does not yet implement Playnite's TTL-based removal policy or all
reference selection/replacement behaviors. The typed GUID launch route uses the
bounded extension connection and ticket protocol, but full reference focus and
fullscreen launch behavior and the remaining extension synchronization features
are still pending. These gaps are not implied by durable settings or catalog
snapshot support.
