# Configuration parity handoff

Reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`.
The inventory contains 242 stable IDs; all remain `pending` in
`configuration-coverage.json` under the strict end-to-end parity rule.
All directly managed Luna agents were stopped before this handoff.

## Current checkout

- The five additional scoped input options (`high_resolution_scrolling`,
  `always_send_scancodes`, `key_rightalt_to_key_win`, `key_repeat_delay`, and
  `key_repeat_frequency`) have input-lease behavior and partial pairing,
  NVHTTP, RTSP, and stream-session wiring. Catalog persistence, signed
  management validation, frontend editing, and an end-to-end scoped test are
  still missing. Do not mark these IDs verified or present the overrides as
  fully available.
- The top-level executable `elevated` setting is admitted by catalog and
  management and reaches launch/application. Windows process creation now
  selects the active console user's token when the host is SYSTEM and keeps
  suspended creation, private job assignment, and resume in that order.
  Non-Windows hosts accept the setting and launch at their normal identity,
  matching the pinned platform behavior. The frontend has no elevation
  control yet. Live SYSTEM, UAC-linked-token, and installed-game tests were
  unavailable on this development host.
- Windows audio policy now keeps the ordinary default endpoint for an empty
  configured sink when host playback is enabled, and selects Steam when host
  playback is disabled or that default is unavailable. Live endpoint and
  multichannel qualification remain open; `/resume` still returns 501.
- `configuration-execution-plan.md` also contains a separately authored
  six-lane Luna Max orchestration proposal. It was preserved for this handoff;
  no worktrees or separate CLI leads were launched by this session. Review
  that proposal against the user's prior Sol High orchestration attribution
  before adopting it.

## Verification at handoff

Passed on Windows: `cargo build --workspace --locked`,
`cargo test --workspace --locked --quiet`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`,
`cargo fmt --all -- --check`, frontend `npm test` and `npm run build`,
`python scripts/inventory_configuration.py --check`, and the real signed
management host test `scripts/test_management_api.py`.
The host test skipped successful provider-owned Playnite override replay
because no finished Playnite snapshot was imported. Windows SYSTEM/UAC and
Linux runtime behavior were not exercised by these gates.

## Next session

1. Finish the five scoped input options at catalog, signed management, and
   frontend boundaries; add precedence and restart tests through the host.
2. Add the frontend `elevated` editor and qualify Windows SYSTEM/UAC process
   ownership with an installed-game fixture before claiming platform parity.
3. Continue the remaining source-backed inventory; `sw_preset` and `sw_tune`
   require a functional software encoder and a distribution choice for x264.
4. Keep coverage IDs pending until their complete runtime, migration,
   management, editor, and platform gates pass.
