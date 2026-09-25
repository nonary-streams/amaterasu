# Playnite provider

The provider publishes only complete, validated extension snapshots to the
fixed `amaterasu.management` catalog owner. Product settings are versioned and
persisted by `amaterasu.configuration`; older documents without
`autosync_remove_uninstalled` load with its host default of `true`, and a
settings update that omits the optional field preserves its stored value.

`sync_categories` and `exclude_categories` compare category names after ASCII
lowercase, so matching is case-insensitive for ASCII letters while non-ASCII
characters remain distinct (for example, the Kelvin sign `K` does not match
ASCII `k`). This follows the pinned reference's byte-oriented lowercase
behavior. The reference sets the process locale to C before applying
locale-sensitive `std::tolower`; no Playnite runtime qualification was run.
Duplicate category entries remain accepted and retained as in the reference
parser. Matching folds each list into a set, so repeats do not change selection.

`sync_plugins` and `exclude_plugins` compare plugin IDs after ASCII lowercase.
ASCII letters match regardless of case, while non-ASCII characters remain
distinct (for example, plugin ID `K` does not match configured ID `k`). This
matches the pinned reference, which sets the process locale to C and applies
`std::tolower` byte by byte. Duplicate plugin IDs remain accepted as in the
reference parser; matching folds the lists into sets, so duplicates do not
change selection.

`exclude_games` applies the same bytewise comparison to Playnite IDs. Repeated
game IDs remain accepted and retained as in the reference parser; matching
folds exclusions into a set, so duplicates do not change selection.

`auto_sync` controls membership reconciliation. When it is enabled together with
`autosync_remove_uninstalled`, the provider sends tombstones only for explicit
Playnite records whose `installed` value is `false`. The catalog applies those
tombstones only after a complete snapshot and removes only unchanged entries
still owned by this provider. A manual refresh with `auto_sync` disabled updates
metadata only for attached provider-owned rows whose IDs appear in the complete
Playnite snapshot. It preserves each row's recorded source flags and does not
add rows, change selection membership, or remove or replace entries. After a
successful refresh, launch eligibility is limited to those matching attached
rows in the complete snapshot. Missing IDs, excluded games, and ambiguous
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

With `enabled` and `auto_sync` both true, the provider starts a background
snapshot when settings load or change, then schedules another attempt 30 seconds
after each start; an active snapshot or catalog transaction can delay the next
attempt. This polling also retries when Playnite is stopped or a snapshot fails.
Vibeshine instead requests a sync when its configuration is applied and
otherwise reconciles snapshots sent by the Playnite extension, which coalesces
library changes with a three-second debounce. The 30-second polling interval
and retry behavior are known timing differences.

The typed GUID launch route uses the bounded extension connection and ticket
protocol. Fullscreen entry is a separate Windows-only launch action, authorized
only to `amaterasu.launch`; management can read or update the persisted
`fullscreen_entry_enabled` setting. It defaults to false, is preserved by older
settings updates that omit it. The setting controls the generated catalog row;
manual `playnite_fullscreen` catalog entries remain launchable when it is false.
Both launch paths still require the provider's `enabled` setting. On Windows,
the provider reconciles the fixed `Playnite (Fullscreen)` row on startup and
settings changes, independent of auto-sync or library snapshots. The row is
provider-owned and cannot be modified or deleted through generic catalog CRUD.
Discovery resolves the current `playnite://` executable association,
requires an existing canonical DesktopApp/FullscreenApp pair in one install
directory, and never accepts a path from the request. A Desktop-only install is
rejected before process creation, avoiding an untrackable session. Start returns
a random ticket and state; lookup is keyed by operation ID, and status/stop are
ticket-bound. Stop posts a bounded close request and waits for the exact
FullscreenApp process to exit. Session tickets are held in memory, so they do
not survive a Playnite module restart; recovery never stops a process based only
on broad discovery after its ticket has been lost. A startup timeout or an
unexpected empty process scan leaves the ticket occupying the single-session
slot; only a correlated stop that confirms the launcher and FullscreenApp have
exited releases it.

Manual `playnite_fullscreen` entries use only a typed launch kind and bounded
`exit_timeout_seconds` (0..=300); callers cannot provide a path or game GUID.
Zero still posts the safe close request and uses a fixed 20-second recovery
grace rather than an unbounded wait. This is intentionally bounded and differs
from ordinary executable timeout semantics.

Remaining fullscreen parity includes the reference helper's active-game
watchdog, return-to-fullscreen behavior, relaunch after game exit, and full
extension-driven focus/cleanup behavior. Current focus retries are based on
authenticated process paths but do not establish those missing lifecycle
guarantees. Playnite selection and age settings have Amaterasu-specific bounds,
so this does not claim every reference input edge is identical.

Focus settings default to 3 confirmed focus attempts, a 15-second timeout, and
`focus_exit_on_first=false`. Zero attempts or zero timeout disables focus;
positive numeric settings accept the nonnegative signed 32-bit range. On
Windows, focus uses bounded status paths from the authenticated extension and
tries at most once per second. A supplied executable path must match exactly;
the install directory is used only when no executable path is available. The
timeout remains fixed for the active game even when status metadata changes,
and retries end on timeout, successful-attempt budget, stop, or disconnect.
Other platforms preserve settings but do not perform window focus. The
reference `playnite_fullscreen_entry_enabled` and per-app
`playnite-fullscreen` launch behavior remain pending because the current
catalog launch contract cannot represent that host launcher action safely.
