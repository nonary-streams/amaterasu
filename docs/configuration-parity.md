# Configuration parity inventory

This inventory establishes the configuration surface to implement against
Vibeshine. It records reference declarations and metadata only; it does not
claim that Amaterasu implements matching behavior.

The machine-readable entries and pinned source locations are in
[configuration-parity.json](configuration-parity.json). Stable IDs use
`global:<key>`, `per_application:<apps.json-key>`, and
`alias:<scope>:<alias>`.
Implementation status lives separately in
[configuration-coverage.json](configuration-coverage.json), keyed by those IDs.
All option and alias IDs start as `pending`; update a status to `verified` only
after its parity evidence is complete.

## Pinned reference and regeneration

The source is the sibling `../vibeshine` checkout at commit
`457237db7e57f9c4be21c35bb07e28d1c100eb7d` on `vibe-test`. Regenerate the
ledger with:

```powershell
python scripts/inventory_configuration.py
```

Use `python scripts/inventory_configuration.py --check` to fail when the checked-in
ledger no longer matches the pinned source, or when the status overlay omits a
reference ID or contains an unknown ID. CI checks out the pinned Vibeshine commit
separately for this check.

The extractor reads source with `git show`; it does not copy source files. It
fails if the pinned commit is unavailable, any entry lacks a domain or source
reference, or the backend and web runtime-override allow-lists diverge.

## Coverage

The pinned inventory contains 197 global setting keys, 41 per-application
`apps.json` fields, 103 per-application/per-client override keys, and 4
explicit legacy aliases. It records 193 global parser keys, 172 configuration
guide headings, and 188 web settings or integration policy keys. All 238
inventory rows have an assigned type; 164 include a web-supplied default and
32 include a pinned host default from a specialized integration struct or
the authentication configuration aggregate.

The backend and current web editor have identical lists for the 103 keys that
may be overridden per application or client. These keys inherit the global
setting type and default; the ledger records their separate override scopes.

## Known source mismatches

The Playnite integration has two different UI and host defaults. Both values
are preserved in the JSON so implementation can make an explicit decision:

| Setting ID | Web schema default | Host default |
| --- | ---: | ---: |
| `global:playnite_autosync_delete_after_days` | `0` | `14` |
| `global:playnite_recent_max_age_days` | `0` | `30` |
| `global:session_token_ttl_seconds` | `86400` | `7200` |

The session-token host default is two hours in Vibeshine's configuration
aggregate and its guide. The web extended-settings default is 24 hours. The
inventory preserves both; matching host behavior uses the two-hour default
until a saved override is present.

The coverage arrays list parser keys without a guide heading or web setting,
and web settings without a global parser. These include consumed legacy HDR
workaround keys, backend-only flags and integration toggles, the Linux DS5 MAC
setting, and Playnite installation path fields. They remain in the inventory
because one source surface alone does not establish whether an item is a
current editable setting, compatibility input, or integration-only control.
