# Configuration parity execution workflow

## Current session override: native agents, Ultra

On 2026-09-29 the user requested execution in one session at Ultra reasoning
and clarified that delegation must use this session's native agents API rather
than separate `codex exec` processes. This overrides the CLI launcher and model
tiers below. Keep six logical feature assignments, their build-enabled worktrees,
independent target directories, review evidence, and continuous landing policy.

The native API permits three child threads and counts completed threads against
that limit. Reuse those threads for successive feature assignments; synchronize
only an idle, clean destination worktree before changing assignments. Do not
launch external Codex sessions to bypass this limit. The current Linux host has
16 logical processors and approximately 62 GiB RAM; each build uses two Cargo
jobs. Host-global device and service checks remain serialized.

The session's worktrees are
`/home/chasep/sources/worktrees/amaterasu-20260929-ultra-feature-N_large`, with
matching build directories under `/home/chasep/sources/builds` and manifests in
`/home/chasep/sources/worktrees/.large-manifests`. Evidence and ready handoffs are
under `amaterasu-config-runs/20260929-ultra-native/team-N`. Main remains the sole
landing destination. Preserve the historical workflow below as context rather
than restarting its old CLI sessions.

## Active policy: six coordinators, three workers each

The user's clarification on 2026-09-25 requires MULTIPLE coordinators. Preserve
six autonomous feature coordinators, each with up to three implementation/review
workers (up to 18 workers total). The temporary three-direct-worker topology was
an incorrect interpretation and is superseded. Do not reduce coordinator count
without asking the user. Keep teams supplied with concrete implementation work.

One central integrator manages the short Git landing queue and actual cross-team
collisions. It does not author every task, act as the build server, repeat every
team's review, or approve routine path edits. All six coordinators run independent
CLI sessions so each has its own native worker capacity. Workers do not delegate.
Use fresh coordinator sessions and concise task state; do not replay the old
20-hour coordination history or restart the old non-buildable-lane scripts.

## Ownership and feature delivery

Give a team a complete observable behavior, acceptance tests, stable IDs, an exact
base and all required paths across layers. Keep assignments disjoint across teams.
Within that feature the coordinator can freely assign supporting code and tests
to its workers, without requesting each file from the central integrator.

Have one worker own a behavior across layers whenever practical. Use the remaining
workers for other independent increments and review. Do not require all three to
finish before publishing ready work. Teams review, repair and validate their own
changes and keep coding while another team's commits land.

If teams need the same file, agree on an interface or transfer that scope once;
only the conflicting work waits. Do not use permanent directory ownership to
split a single setting across five coordinators. Shared schemas are explicit
prerequisites and have one current owner. Existing source boundaries are initial
reservations, not requirements to parcel a feature into disconnected fields.

Prefer completing existing partial features. A blocked platform gate must name
the missing hardware or environment; it does not prevent finishing local behavior
or other independent features. Keep coverage verified reserved for complete parity.
Track implementation, local validation, review, landing and platform qualification
separately so real progress remains visible without overstating completion.

## Build-enabled teams and workers

The user approved build-enabled worktrees. Team and child worktrees use _large
directory names and manifest-recorded isolated build directories. The
Worktree provisioner creates them. Each child gets its own checkout and
CARGO_TARGET_DIR=<manifest BuildDirectory>/cargo-target, with CARGO_BUILD_JOBS=2.
Children must not inherit their coordinator's target directory for compilation.
Use Cargo's common download cache; never share mutable target directories.

The current machine has 24 logical processors and about 64 GB RAM. The check
helper allows six simultaneous validation jobs (two Cargo jobs each); further
checks wait automatically. This limits build contention while all teams continue
implementation. There is no single global build queue serviced by a model.
Host-global device/port/service tests remain explicitly serialized.

Start a team with scripts/run_configuration_worker.ps1 -Coordinator -Slot N.
The runner accepts slots 1 through 6, checks checkout and base state in code,
locks the team slot, enables three native workers and writes structured logs.
Coordinator restarts may resume a dirty checkout after inspecting its paths;
standalone workers require a clean checkout at launch.
Without -Coordinator it remains a non-delegating standalone worker runner.
Coordinators are pinned to GPT-6 Sol XHigh and their new native workers are
pinned to GPT-6 Sol Medium. The runner rejects model or reasoning-effort
overrides that change those tiers.

Models cannot be changed inside an already-running `codex exec` session. If a
legacy coordinator is still running, ask it to stop assigning new work and let
its active workers finish, validate, and commit. It records their SHAs, checks,
unfinished work, and reservations in the team mailbox before exiting. The root
integrator relaunches each available slot independently as Sol XHigh on its
existing team worktree. The new session reads STATUS.json, INBOX.json,
READY.json, and worker commits before dispatching Sol Medium workers. Never
kill or duplicate active workers to switch models.

Run local validation with:

```powershell
./scripts/check_configuration_slice.ps1 -Packages amaterasu-catalog,amaterasu-launch
# Add -Frontend when changing the editor.
```

Workers run focused tests, rustfmt and strict all-target Clippy before handoff.
Failures stay with the owning worker/team. No identity-only bootstrap agent,
SQLite model-verification round trip, or lightweight-worktree test prohibition.
The coordinator verifies checkout state as part of normal dispatch and validates
its integrated team branch after changes interact or conflict.

## Review and continuous landing

Use Commitlog with actual session attribution. Commit coherent validated increments
as they finish. Preserve historical attribution; new work names its actual team
and model when appropriate. Do not carry obsolete Luna orchestration footers
from earlier task notes into new commits. Do not manufacture status-only commits
for throughput.

One independent review within the team is sufficient for ordinary changes. The
coordinator can review worker-authored code; a worker reviews coordinator-authored
code. Use a focused independent review for security-sensitive contracts. Publish
READY.json in the team's external mailbox with base/head, commit SHAs, changed
paths, test commands/results, reviewer and remaining platform gates.

The central integrator checks dependencies, ownership, clean Git state, and the
team's evidence, then lands ready commits promptly. Do not re-run the same review
and unchanged package tests at main. Revalidate when integration changes code,
when consumers changed since the tested base, or when a shared contract requires
it. Run full workspace gates at coherent checkpoints. A failure returns to its
team; it does not force all six teams to wait.

Main has one Git writer. Merge a validated branch (fast-forward when possible)
or cherry-pick a reviewed legacy candidate. Preserve all old branches and dirty
work. Synchronize only idle clean worktrees; never reset an active writer. Serial
Git updates are short and must not serialize implementation, builds or reviews.

## Progress and operating thresholds

Each team publishes ready commits directly and maintains one concise status in
its external mailbox. The integrator reports newly landed SHAs and blocked teams.
Use scripts/show_configuration_progress.ps1 -RunDirectory <team-run-directory>
to show all six coordinators, activity and unlanded team commits.

After 15 minutes without implementation progress, identify the concrete blocker.
After 30 minutes without a ready or landed increment, inspect the dependency,
validation or review queue and unblock it. These are intervention thresholds,
not an excuse to commit broken code. Idle workers take another useful bounded
slice or review; do not keep rediscovering the complete inventory.

## Prepared team roots and dispatch

Team roots are D:/sources/worktrees/amaterasu-feature-N_large, N=1..6. Their
initial branches are config-throughput/feature-N_large; a coordinator may
continue on a slot-specific feature-N respin branch when its manifest records
that exact current branch. Relaunch the preserved clean checkout without
switching it back or discarding commits. Child worktrees are
D:/sources/worktrees/amaterasu-team-N-worker-M_large, M=1..3, with branch
config-throughput/team-N-worker-M_large. Their manifests are under
D:/sources/worktrees/.large-manifests. Build directories are the matching names
under D:/sources/builds. Reuse these checkouts/caches after clean synchronization.

All teams use D:/sources/worktrees/amaterasu-config-runs/teams-20260925 for
coordinator locks and logs. Each gets a team-N mailbox and its own task file with
current reservations, child paths and acceptance criteria. Publish readiness to
that mailbox without waiting for the central integrator to approve local progress.

The prior direct worker's pairing correction remains valid and should land after
review. Historical lane sessions, handoffs and worktrees are preserved. The old
run/resume scripts remain disabled because they enforce the obsolete centralized
validation workflow; launch six NEW build-enabled coordinator sessions instead.

Pinned reference: Vibeshine 457237db7e57f9c4be21c35bb07e28d1c100eb7d.
Use it as behavioral evidence without copying or translating its source.

## Existing implementation notes

These notes describe partial behavior from the previous run. Confirm against the
current source before selecting the next feature; they do not override the active
workflow above.

The authentication TTLs accept 1 through 2,147,483,647 seconds. Amaterasu stores
them in revisioned v1 product settings; authentication tests cover bounds, CAS,
restart loading, refresh rotation, and expiry. Host defaults are 7,200 seconds for
`global:session_token_ttl_seconds` and 604,800 seconds for
`global:remember_me_refresh_token_ttl_seconds`. Vibeshine's host defaults match,
but its web settings merge shows 86,400 seconds for an unset session TTL. Its
secure HTTP-only cookies and persisted server-side session state survive browser
reload and host restart. Amaterasu now persists session digests in a versioned
private sidecar and restores remembered browser sessions under a Web Locks guard.
It still uses explicit bearer tokens instead of the reference's cookie flow;
live multi-tab and installed-host restart qualification and the web fallback
difference keep both IDs pending.

Session-history settings now use the central `amaterasu.configuration/1`
revision for signed compare-and-swap updates. Startup migrates older SQLite
history settings and leaves recording disabled until that product setting is
confirmed. The stream lifecycle queues bounded, nonfatal begin/end writes and
releases each result without holding up media work. The signed editor can
change retention policy and read it after restart. Runtime stream metrics and
remaining Vibeshine history semantics still require implementation and
qualification, so the history IDs remain pending.

Host bootstrap now parses global `min_log_level` and `log_path` and passes a
typed policy into diagnostics. Accepted diagnostic records above the selected
level reach a bounded per-session disk log; relative paths stay beneath the
private state root. A signed host test checks level filtering and configured
file output. These IDs remain pending because other host and native-library
logs do not use this sink, updates require a worker restart, and prerelease
defaults currently follow the host crate version rather than the product
release channel.

Legacy migration now follows configured `file_state` and
`vibeshine_file_state` auxiliary paths when they resolve to contained regular
files. Missing auxiliary leaves are skipped after their existing ancestors are
checked; escaping, dangling, or conflicting paths are rejected. The package
snapshot, pairing identity importer, and administrator credential importer
apply the same containment rule, while nonempty legacy API tokens remain a
preflight blocker. This preserves more predecessor state without making the
configured paths live Amaterasu settings. `file_state`,
`vibeshine_file_state`, `credentials_file`, `cert`, and `pkey` remain pending;
the Unix symlink cases also need execution on Unix CI.

`global:origin_web_ui_allowed` remains pending. Vibeshine recognizes the exact
lowercase `pc|lan|wan` values, defaults to `lan`, and checks the remote peer IP
against that maximum tier: loopback is PC; RFC1918, CGNAT, link-local, and IPv6
ULA addresses are LAN; other addresses are WAN. This is separate from the
browser `Origin` header, and `/welcome` is an early request-policy exception.
Amaterasu's management service is disabled by default and accepts only loopback
binds and peers over plain HTTP, so its effective policy is PC. Its
`allowed_origins` setting is a separate browser-origin check. Do not expose LAN
or WAN peers until management has TLS ingress. Vibeshine also maps its
configuration UI port through UPnP only in WAN mode when UPnP is enabled;
Amaterasu has no UPnP mapping. The pinned implementation is in `src/config.cpp`,
`src/network.cpp`, `src/http_auth_request_policy.cpp`, and `src/upnp.cpp`.
These transport and exposure gaps keep this ID pending.

Each worker commits its owned implementation after rustfmt, meaningful tests,
and Clippy. Use the repository's Nonary Git identity and actual session model
and effort for new commit attribution. Preserve attribution in historical
commits; do not rewrite it to match the new launch policy.

Steam catalog reconciliation now owns explicit exclusions, app type selection,
recent-play selection, and the policy for retaining stale entries in all-game
sync. It reads local app metadata and play history before changing the catalog;
an incomplete scan leaves the prior catalog intact. Remote artwork, richer
reference metadata, and platform-specific discovery and launch behavior still
need verification. The Steam IDs remain pending until those effects are covered.

The Steam importer now discovers bounded local artwork and persists opaque
provider references instead of filesystem paths. The signed `steam.read`
endpoint serves fixed-size image chunks; the catalog editor assembles and
displays those chunks with revision and size checks. This covers local artwork
discovery and browser display. A fixed official Steam CDN portrait can now be
fetched when no local artwork exists, through a single bounded worker and
revision-checked signed reads. The transfer accepts the reference's 16 MiB
remote image limit. The provider has a bounded persistent cache with recovery
for interrupted writes, but the host has not yet supplied its trusted private
cache root; packaged restart persistence remains unqualified. Low-resolution
local art is not yet replaced with converted remote art as in the reference.
Local discovery ranks bounded portrait candidates by parsed resolution across
configured Steam roots.
Platform qualification remains pending. Manual catalog entries can preserve explicit
Steam ID and install-directory metadata. On Linux, a launch snapshots a
baseline of same-user processes and associates new process identities only
with executable or working-directory evidence inside the canonical install
tree, plus their descendants. Stop signals only tracked identities through
pidfds after revalidation; the Steam launcher/client is never signaled.
Without a usable install directory, liveness remains unknown and stop only
releases the stream. On Windows, the tracker likewise associates only new
same-user game processes with canonical install-tree image evidence and
creation-time identity, expands to verified descendants, and rechecks the
same process handle before a graceful window close or forced stop. Native
tests cover both stop paths and unrelated processes. Linux runtime
qualification and Gamescope capture-source handoff remain pending.

The global base port now derives HTTP, HTTPS, RTSP, and streaming UDP ports.
IPv4 and IPv6 listeners can coexist where the startup policy requests both.
`global:port` remains pending until listener moves, startup recovery, and
platform behavior are verified as one configuration contract. Durable live
updates of `address_family` and `bind_address` now cover NVHTTP and RTSP on
Windows. Those IDs remain pending until every public listener and supported
platform follows the effective policy without weakening the loopback control
plane.

The configured host name now reaches `/serverinfo` and seeds each new stream's
device name. Moonlight's optional `clientName` overrides it for that launch,
and RTSP forwards the resulting label to stream-session history separately
from the capture output. Legacy launch callers retain the output fallback.
Vibeshine also uses `sunshine_name` when naming a launch-owned virtual display;
Amaterasu's virtual-display leases currently have no launch/session ownership
and retain caller-supplied labels. That lifecycle and platform discovery
qualification keep `global:sunshine_name` pending.

The `external_ip` text field now persists in product document v12 and has a
signed editor. Omitted updates preserve the value; null or empty resets to
automatic, and nonempty text is retained exactly within the host's bounded
input policy. The pinned Vibeshine source parses and displays this field but
has no runtime reader; its documentation promises automatic detection when
blank. Address detection and advertisement remain pending in Amaterasu, as
does qualification of the source's unbounded text acceptance.

`csrf_allowed_origins` remains pending. It is a cookie-session CSRF bypass
allowlist in the reference, whereas Amaterasu uses scoped bearer authorization.
The existing CORS `allowed_origins` setting does not grant authentication.

Per-application process lifecycle fields now preserve omitted values and apply
explicit values at launch. `fec_percentage` resolves from the global stream
profile through a sparse app override and a certificate-matched paired client
record. `stream_audio` now defaults on, persists globally, and resolves an
optional app value and then an optional certificate-matched client value for
each launch. A disabled session skips audio encoder and capture resources while
retaining the negotiated audio lane and completing its media drain. The
management editor saves the global value and sparse app/client overrides;
explicit `false` survives catalog and pairing persistence. Provider-owned
Playnite entries can edit both overrides through a narrow catalog operation
that preserves provider launch identity and survives refresh. Successful
signed Playnite editing still needs a completed Playnite snapshot fixture.
Global `audio_sink` persists in product document v10 and selects the exact
loopback capture endpoint for each new stream. Sparse app and
certificate-matched client overrides resolve client, then app, then global,
then startup endpoint. An explicit empty scoped override chooses the startup
automatic endpoint, while absence inherits. Windows default-role switching
now uses a lease for an explicitly selected `audio_sink`: when
`audio_sink_capture_only` is false it temporarily selects that endpoint for
Console, Multimedia, and Communications, then restores each role still owned
by the lease. The flag persists globally and as sparse app/client overrides,
and the resolved value reaches Audio Open for each stream. A failed restore is
reported and retried before another routed Open. Live Windows endpoint and
role-transition qualification, virtual-sink behavior, and audio endpoint
qualification on other supported platforms remain pending. The other shared
override keys and their runtime consumers remain pending.
Product document v15 now stores `virtual_sink` separately from `audio_sink`.
Signed global, application, and certificate-bound client values preserve an
explicit empty string; each new stream resolves client, then application,
then global. A nonempty selector bypasses regular endpoint routing and
capture-only mode, and Windows AudioCapture matches the selected endpoint by
ID, friendly name, description, then adapter name. The backend tries the
reference format candidates against the current default render bit depth and
captures the selected device as 48 kHz float PCM. Stream-session and Opus
remain stereo-only even though the backend can select 5.1/7.1 layouts.
Windows AudioCapture now selects an available Steam stereo endpoint when host
audio is disabled or the ordinary default endpoint is missing, falling back to
the ordinary endpoint if Steam is unavailable. A Moonlight
`localAudioPlayMode` value of 0 or 1 reaches AudioCapture through the RTSP and
stream-session snapshot; omission defaults to 0. The `/resume` endpoint is
still unsupported. Hardware and cross-platform qualification, multichannel
streaming, and complete resume behavior remain pending for
`global:virtual_sink` and its scoped overrides.
Request-supplied client IDs are not used as authority. The editor also
preserves explicit executable lifecycle values and preparation steps.
Global preparation now persists as a bounded structured list in NVHTTP product
document v16 and is copied into each admitted launch. The launch module puts
those steps before application preparation unless the catalog entry sets
`exclude_global_prep_cmd`. Playnite and Playnite Fullscreen launches use a
prepare-only application lease: provider start waits for preparation, and
provider stop is confirmed before reverse undo. A failed undo retains the
session for retry. Vibeshine's `global_prep_cmd` accepts raw `do`/`undo` command
strings, an `elevated` flag, and `$()` environment expansion; Amaterasu's
structured executable/arguments form does not yet implement those semantics.
`global:global_prep_cmd` and `per_application:exclude-global-prep-cmd` therefore
remain pending despite the working structured lifecycle.
The launch default for an omitted per-application `exit-timeout` now matches
Vibeshine's 10 seconds; an explicit zero remains distinct. Structured catalog
executable and working-directory paths now expand bounded `$()` references,
trim matching outer quotes, and resolve bare executables through absolute PATH
entries. Process parity still requires the reference's raw command and elevated
launch semantics, expansion in other command fields, full Windows quote/PATHEXT
handling, relative PATH entries, and the source undo timeout behavior.

The Windows global capture output can now be selected by exact provider ID
and persists through host restart. A launch snapshots that output into RTSP
and fails when the selected ID is unavailable. The editor lists discoverable
outputs and retains a saved ID even when discovery is unavailable. Automatic
uses the bootstrap output. Output selection on other supported platforms and
the complete capture-provider selection policy still need verification.
The WGC backend now supports an explicit constant-rate mode that reuses the
last owned frame on an idle read with monotonic frame metadata. Global capture
modes `auto`, `ddx`, `wgc`, and `wgcc` persist and route through each stream
launch; explicit WGC modes fail when that provider is unavailable. The `auto`
mode retains the startup provider. Vibeshine's virtual-display/game-context
selection for `auto`, per-app/client capture overrides, and Linux/macOS capture
support remain pending.
The Windows WGC pacing option now defaults enabled, persists as a nullable
product document v11 setting, and snapshots into Video Open. After a missed
capture slot, ordinary WGC streams keep the prior frame grid only when the
next fresh frame lands within the source phase and hold bounds. A versioned
capture v1.2 callback now distinguishes newly acquired WGC frames from
constant-rate cached reuse, so cached deliveries advance the existing grid
without creating a false fresh-frame anchor. Older capture providers keep the
v1.0/v1.1 path. The nonblocking capture pump still cannot reproduce the
reference's 200 ms blocking wait, and live Windows timing qualification
remains pending.

The media transport now caps each software video send batch to a selected
16, 32, or 64 KiB frame allowance. Product document v7 persists the nullable
host setting, migrates older documents to the 64 KiB default, and snapshots
the effective value through RTSP and stream session on launch. The signed
editor preserves raw 0..64 selections while reporting the effective fallback.
The transport also attempts Windows UDP segmentation or Linux UDP GSO for
eligible batches and falls back to ordinary datagrams when the socket or
platform does not support it. Driver, NIC, and live Linux behavior still need
qualification before this setting can be marked complete.

`global:sw_preset` and `global:sw_tune` remain pending. Vibeshine defaults
them to `superfast` and `zerolatency` and passes their strings to x264/x265
through FFmpeg; its parser accepts strings beyond the web editor's listed
choices. Amaterasu's encoder module currently enumerates Windows hardware
H.264 MFTs and has no software encoder consumer on any platform. Carrying
these values through settings alone would not affect output. A software
encoder must preserve the current NV12 input, Annex B output, timestamps,
forced IDR, and drain contracts. Exact preset/tune behavior requires an x264
backend and a deliberate dependency and distribution choice before the
settings can be presented as functional.

The `ping_timeout` setting now has a signed editor and nullable product
document v13 value. Its 10,000 ms default and 0..2,147,483,647 ms range are
snapshotted at launch, carried through RTSP and stream preparation, and used
for post-prepare expiry, authenticated control idleness, and UDP peer discovery.
The editor's blank/-1 resets to the default. Cross-platform client timing and
live-stream qualification remain pending; this ID is not yet verified.

The `max_bitrate` host option now stores a signed raw value in product document
v8. Positive values cap Amaterasu's initial configured stream bitrate; zero and
negative values mean no configured cap. The current encoder profile accepts
100..100,000 kbps, so a positive cap below 100 kbps rejects the launch rather
than exceeding the configured cap. Vibeshine's live `/bitrate` update, higher
absolute ceiling, client-requested bitrate parsing, and encoder-specific
range behavior remain pending. The
`minimum_fps_target` source initializer is 20 even though its documentation
lists zero. Product document v9 stores a nullable 0..1000 finite target,
snapshots the effective value at launch, and uses an owned accepted-frame
cache to repeat idle video with increasing frame metadata. Zero follows the
negotiated stream FPS. New captures take precedence; busy encoder submissions
do not cause an unbounded repeat burst. Live changes apply to future streams.
Platform-specific encoder and capture timing still need qualification.

Keyboard and mouse settings now persist and gate native input events; disabling
a category releases its held keys or buttons and reports pending cleanup when
the operating system rejects a release. High-resolution scrolling now
preserves Moonlight wheel units on Windows and emits Linux high-resolution
wheel events with compatible detents. It has a signed management editor, but
Linux runtime qualification remains pending. The scancode option now follows
raw Moonlight key mode on Windows and preserves the native key release plan.
It also has a signed management editor. Right Alt can now map to the Windows
key with stable key-up behavior and a signed editor. Lease-scoped keyboard
repeat now applies configurable delay and frequency, including zero-delay
disable and legacy document migration, with a signed editor. Vibeshine also
accepts all seven keyboard, mouse, scrolling, scan-code, Right Alt, and repeat
settings as per-application and per-client overrides, with client values taking
precedence over application values. Amaterasu now resolves scoped `keyboard`
and `mouse` values at input lease acquisition, preserving explicit false.
Scrolling, scan-code, Right Alt, repeat, and the other pinned advanced input
overrides are not yet scoped. Scroll remainder handling across policy changes,
per-client repeat timing, and the `keybindings` precedence over the Right Alt
alias differ as well. These seven IDs remain pending.

### Controller and gamepad implementation sequence

`global:gamepad` and `global:back_button_timeout` remain `pending` in
`configuration-coverage.json`. The GameStream controller packet decoder
already produces a validated snapshot containing a controller number (0..15),
the client's full active-controller mask, button flags, triggers, and four
axes. The translator now preserves that complete state through a bounded JSON
event and input model. On Linux, stream sessions forward snapshots only after
the input service probes a working uinput controller device. Lease-owned slots
then create and update up to 16 native controllers, remove slots absent from
the active mask, and retain failed cleanup for retry. Controller failures are
nonfatal to the stream. The Linux input service now persists a global
`back_button_timeout` default of -1 ms and applies a per-slot Back/Select
deadline. At expiry it suppresses Back and pulses Guide for 100 ms, cancelling
the timer on release or slot teardown. Failed native updates retain a retry
state. Windows now uses a dynamically loaded ViGEm X360 client and probes
the bus at lease start; keyboard and mouse remain usable if the driver or
client DLL is absent. No DLL or driver is bundled. macOS reports native
injection as unsupported. Linux still needs runtime device and permission
qualification, and the reference controller profile choices are incomplete.
Sparse application and certificate-bound client overrides for Back timeout,
controller, gamepad, keyboard, and mouse now resolve client, then application,
then durable global policy at input lease acquisition. The exclusive lease
snapshots its effective values, so later global edits apply to later leases;
sequential owner/profile restoration is covered by tests.
The input product document v7 now persists a platform-specific `gamepad`
choice. Versions 1 through 6 migrate to `auto`. Leases snapshot the requested
and effective profile; status reports backend-supported choices and an
unavailability reason. Windows implements `auto` and `x360` through ViGEm;
Linux implements `auto` and `xone` through uinput. The remaining pinned choices
are recognized but cannot inject controller packets. The signed management
route and editor preserve profile choices and show lease-time availability.
Native runtime qualification and missing profiles still block
full gamepad parity.
Input product document v8 also persists the default-enabled `controller`
switch. Disabling it suppresses controller snapshots while leaving keyboard
and mouse input active; existing virtual slots are removed immediately, with
failed removal retained for retry. The signed editor and status expose the
policy and pending cleanup. Its application and paired-client overrides now
reach the input lease. Native device qualification remains pending.

The live path follows this order; steps 1, 2, and 4 plus initial Linux and
Windows backends in step 3 are implemented:

1. Preserve each decoded controller snapshot through the GameStream translator
   and stream-session queue into the input service, including the complete
   active mask and signed axis values. Bound queued snapshots and keep controller
   state separate from keyboard/mouse transitions.
2. Add lease-owned slots for up to 16 controllers in the input module. Allocate
   only for active slots, update a complete native state, and neutralize and
   destroy slots removed by the full active mask, lease release/expiry, stream
   failure, or module shutdown. Retain ownership when cleanup fails so release
   can be retried and reported.
3. Implement at least one genuine virtual-controller backend before exposing a
   supported `gamepad` choice. Report only profiles the installed backend can
   create; later add the pinned reference's automatic profile selection and
   remaining per-platform choices. Keep unsupported profiles and platforms
   explicit rather than returning successful no-op injection.
4. Add `back_button_timeout` to the live input policy after snapshots reach a
   real backend. A negative value disables the behavior. On a Back/Select press,
   schedule a per-slot deadline; cancel it on release. At expiry, release Back,
   pulse Home/Guide for 100 ms, then suppress Back until a later client snapshot
   reports it released. Removing the slot or ending its lease cancels its timer
   and resets reconciliation state. Use the input module's regular tick and
   per-slot deadlines so a stale asynchronous callback cannot affect a reused
   controller slot.

The pinned reference is Vibeshine commit
`457237db7e57f9c4be21c35bb07e28d1c100eb7d`. Its `gamepad` default is `auto`.
Windows automatic selection prefers a working ViGEm backend and falls back to
Vibeshine's VHF/UMDF backend only when ViGEm is unavailable; explicit VHF
profiles require that driver and do not silently substitute another profile.
VHF availability therefore requires a real installed, trusted driver package
and its user-mode client protocol. ViGEm is a legacy driver prerequisite and
should not be introduced as an unqualified dependency. Linux's reference choices
are `auto`, `xone`, `ds4`, `ds5`, and `switch`, implemented through Inputtino
virtual devices. A native Linux implementation also requires a working kernel
uinput/UHID path and service-account permissions to create those devices.
Amaterasu now has a probed Linux uinput Xbox-like output, but does not yet
implement the other reference profiles or qualify the device path on a Linux
host. Windows ViGEm X360 integration needs installed-driver and device
qualification; VHF/UMDF remains unimplemented.
The pinned macOS backend has no virtual gamepad implementation; keep it
unsupported until a genuine macOS virtual HID provider is selected and tested.

The reference timeout default is -1 ms. Tests should cover packet-to-service
preservation, multi-slot active-mask changes, native allocation and update
failure cleanup, lease teardown, timer behavior just before and at the deadline,
release-before-expiry cancellation, the 100 ms Home/Guide pulse, Back-state
reconciliation, and timer cancellation before slot reuse. Pure protocol and
controller tests do not qualify native delivery: Linux still needs runtime
uinput/UHID device and permission checks; Windows needs builds against the
selected driver ABI plus an installed-driver integration gate. macOS remains
an explicit unsupported-platform gate. Neither configuration ID can become
`verified` until its management update, persisted default/validation,
stream-to-device effect, cleanup behavior, and supported platform gates pass.

Playnite now imports only complete, bounded extension snapshots through its
own provider. Explicit uninstalled IDs can remove still-owned catalog entries
when automatic synchronization and its durable cleanup setting are enabled;
missing, excluded, and incomplete records cannot imply uninstall. Its settings
and status have a management editor. TTL cleanup uses first-import timestamps
and only expires eligible, unplayed Playnite entries; the default is 14 days,
and zero disables this policy. Recent-selection cleanup can require a bounded
replacement count, with both policies tied to a revision-checked provider
inventory and reason-specific, replay-safe tombstones. Typed GUID launches use
authenticated extension IPC and hold the session lease until correlated stop
proof. Windows focus retries use authenticated game-status metadata, exact
executable matching when supplied, and a fixed bounded deadline with
cancellation on stop. A typed Playnite Fullscreen target now supports manual
catalog entries and an optional provider-owned generated row with correlated
start, status, stop, and recovery. Active-game watchdog, return-to-Fullscreen
behavior, complete extension management, and Windows process verification
remain pending.

Lutris now imports complete current-user Linux library snapshots with
exclusions and optional Steam deduplication. Lutris settings and status have a
signed management editor;
complete scans now remove currently discovered entries that become excluded,
non-importable under Steam policy, or shadowed by direct Steam entries even
when stale removal is disabled. Explicit excluded IDs also remove older rows
absent from the scan, while unrelated disappeared entries remain retained.
Name-only exclusions also remove matching absent rows through a Lutris-authorized
catalog inventory of attached names, pinned to the collection revision used to
begin the snapshot. Detached rows and other providers' entries remain untouched.
Cross-session discovery and generic-launch eligibility after disabling the
provider remain pending.
