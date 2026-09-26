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

With `enabled` and `auto_sync` both true, one authenticated Sunshine-role
connection receives the extension's initial and debounced library snapshots.
Complete snapshots from the current Playnite process start catalog reconciliation;
incomplete snapshots, disabled settings, and stale process identities cannot.
Snapshot, launch, and stop commands share the same connection and reader.
The provider also schedules a snapshot request every 30 seconds as a retry when
an event is missed, Playnite is stopped, or a snapshot fails. An active snapshot
or catalog transaction can delay the next attempt.
Discovery of a newly started or restarted Playnite process requests an immediate
retry, subject to an active snapshot or catalog transaction finishing first.
Launch eligibility is tied to the discovered Playnite process IDs and paths.
When Playnite stops or restarts, previously imported game IDs cannot launch
against the new extension instance until a snapshot from that instance has
completed catalog reconciliation. A snapshot or catalog completion from the
previous instance cannot restore eligibility.
Discovery changes also cancel an unfinished catalog reconciliation, so entries
from an earlier Playnite instance are not submitted after a restart. A request
already accepted by the catalog is drained before the next snapshot starts.
Manual refresh forces current process discovery, including during the normal
discovery throttle. Focus and Fullscreen entry setting changes preserve launch
eligibility from the current completed import, including when auto-sync is off;
disabling the provider revokes it. Automatic selection policy changes revoke it
when auto-sync is on. An in-flight import continues across operational settings
changes but is canceled on provider disable, policy change, or process restart.
The Amaterasu extension in `extension/` coalesces library changes with a
three-second debounce. It captures complete Playnite records on the UI thread,
then places an immutable snapshot batch in a bounded queue owned by one pipe
connection. Each bracket contains the fresh nonce from that connection's
authenticated hello, and requested brackets also echo a request ID. A queued
batch from an old connection cannot acquire a new nonce. Missing, mismatched,
or partial brackets cannot update the catalog. A library change whose dirty
generation is observed before completion cancels the bracket and recaptures a
requested snapshot with its original request ID. The final completion write
and dirty generation update share a lock; a blocked completion write closes
the pipe after one second. Playnite collection events are asynchronous, so a
mutation reported only after completion may briefly leave an older complete
snapshot visible until the debounced update or 30-second recovery poll. Native
installed Playnite testing must qualify that event ordering. Launch and stop
results are bound to the current connection nonce and command request ID.

Playnite's installed extension directory is `%AppData%\Playnite\Extensions`;
portable installs use `Extensions` below the Playnite installation. To test a
source checkout, copy `extension.yaml` and `AmaterasuPlaynite.psm1` from
`extension/` into an `Amaterasu.Playnite.Connector` child directory there, then
restart Playnite.
The Amaterasu installer does not yet install or roll back this extension;
that packaging work is tracked separately. Only the service-launched
`worker-user-serve` process can claim Sunshine, launcher, or fullscreen roles:
the extension compares
the kernel control/data client PID, live SCM service parent, executable path,
and worker command line. It also requires the SCM image to reside in an
administrator-protected location. This contract requires a packaged Windows/Playnite
smoke test because CIM visibility, pipe ACLs, and UI dispatcher behavior depend
on the actual service and user tokens. Playnite's
[extension introduction](https://api.playnite.link/docs/tutorials/extensions/intro.html)
states that PowerShell script support is planned for removal in version 11;
that version needs a separately qualified extension implementation.

The typed GUID launch route uses the bounded extension connection and ticket
protocol. Snapshot, launch, and stop requests use the extension's typed
`command` envelope; a command result with the current nonce and request ID
confirms acceptance. Stop asks the launcher to end the stream; it does not kill
the game. A recovered session still needs a matching `gameStopped` observation
before cleanup can be called complete.
Fullscreen entry is a separate Windows-only launch action, authorized
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

Each Fullscreen ticket also owns a local authenticated launcher-status watcher.
The helper connects with the generic `role: launcher` / `mode: fullscreen`
hello, and Amaterasu binds every received record to the exact local ticket and
the verified Playnite process image plus Windows process creation FILETIME. The
extension sends active-game sync and `gameStarted` records with a GUID, install
directory, and executable, but the record itself has no ticket, PID, or creation
identity. Only one game can be associated with a ticket; duplicates of that
GUID are idempotent and a second distinct game ID makes the session fail closed.
The exact-executable watchdog checks the full reported image path at most once
per second. It requires prior observation of that exact image, then requires
three seconds with no process of that image before restoration. A matching
`gameStopped` prompts the next scheduled exact-image verification scan, but
does not replace prior process observation. Access-denied and incomplete scans
do not count as exit evidence. Since the extension does not report a game PID or
bind game status to a ticket, a same-path process from another launch delays
return. If a game starts and exits between scans, even a `gameStopped` record
cannot establish that its process is gone; the ticket remains Playing until
manual stop. The pinned extension
normally omits `gameStopped` for games started after a generic launcher
connection; exact-image process observation is the fallback. On shutdown the
extension asks each authenticated Fullscreen connection's sole writer to flush
one `playniteExiting` record before closing pipes, with a 500 ms total wait.
Writer confirmation means a local pipe flush, not receipt by the watcher;
abrupt termination or a blocked pipe can still lose the record. When received,
it suppresses auto-return for an intentional Playnite exit. The ticket becomes
Stopped only after a complete exact FullscreenApp scan
finds no process for that installation. An unknown scan or any remaining
same-install fullscreen UI keeps the ticket active, even when the ordinary
discovery snapshot is empty. This also applies to tickets already in
CleanupFailed. Reconnect is allowed for the same live process identity, while a
restart retires the previous creation-time identity and discards its queued
events.
Return focuses only the ticket's verified FullscreenApp process, or relaunches
from the still-validated canonical install when no uncorrelated FullscreenApp
is present. Startup requires a complete exact-image scan before launch to
establish its baseline. Handoff adoption, automatic return, and stop cleanup
claim an untracked process only from a complete exact-image scan and only when
it is uniquely post-baseline. Coarse discovery cannot authorize focus or close. Scan
errors, ambiguous candidates, and baseline PID reuse fail closed without
focusing, closing, or relaunching. Exact PID/path/creation-time checks still
guard stored identities. Native Windows pipe, process enumeration, focus, and
restart behavior still requires runtime qualification.

Manual `playnite_fullscreen` entries use only a typed launch kind and bounded
`exit_timeout_seconds` (0..=300); callers cannot provide a path or game GUID.
Zero still posts the safe close request and uses a fixed 20-second recovery
grace rather than an unbounded wait. This is intentionally bounded and differs
from ordinary executable timeout semantics.

Playnite selection and age settings have Amaterasu-specific bounds, so this does
not claim every reference input edge is identical. The fullscreen helper still
cannot attribute a game process to the reported GUID because the extension
protocol supplies neither a PID nor a ticket for lifecycle statuses.

Focus settings default to 3 confirmed focus attempts, a 15-second timeout, and
`focus_exit_on_first=false`. Zero attempts or zero timeout disables focus;
positive numeric settings accept the nonnegative signed 32-bit range. On
Windows, focus uses bounded status paths from the authenticated extension and
tries at most once per second. A supplied executable path must match exactly;
the install directory is used only when no executable path is available. The
timeout remains fixed for the active game even when status metadata changes,
and retries end on timeout, successful-attempt budget, stop, or disconnect.
Other platforms preserve settings but do not perform window focus.
