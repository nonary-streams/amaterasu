# Vibeshine configuration parity execution plan

Sol owns integration, cross-module contracts, security review, and
final verification. Luna XHigh workers take bounded domains with disjoint file
ownership. Each worker uses the pinned Vibeshine commit in
[configuration-parity.md](configuration-parity.md) as a behavioral reference,
without copying or translating its implementation.

## Definition of complete

For every stable ID in [configuration-coverage.json](configuration-coverage.json),
record the reference default, accepted values, validation and normalization,
platform conditions, persistence and migration behavior, every runtime consumer,
management authority, frontend edit behavior, and a meaningful regression test.
Mark an ID `verified` only after these match Vibeshine. A stored value or an
editor control alone is incomplete. Verify legacy aliases and every supported
application/client override independently. Keep genuinely unsupported behavior
`pending` and name the missing consumer or platform gate.

## Work sequence

| Phase | Luna XHigh assignment | Sol integration gate |
| --- | --- | --- |
| 1. Reference contract | Inventory parser, guide, web, integration, application and override settings; pin sources and defaults | Audit stable IDs, conflicts and omissions; run the inventory drift check in CI |
| 2. Settings foundation | Extend typed schema, caller-owned product documents, versioned updates and editor controls | Review authority, migration, CAS ambiguity, result release and concurrent request progress |
| 3. Small live slices | Assign one settings family per worker, including every relevant runtime consumer | Signed management test, restart persistence, frontend typecheck and domain regression |
| 4. Core streaming | Network, security, video, audio, input, capture, display and encoder families | Platform-specific negotiation and live-stream behavior; avoid storage-only parity claims |
| 5. Integrations | Steam, Playnite, Lutris, frame limiter, update, tray and diagnostics families | Provider reconciliation, launch/session behavior and platform defaults |
| 6. Application overrides | Per-application fields and all shared per-client override keys | Precedence, alias migration, launch effects, multi-client isolation and conflict tests |
| 7. Final audit | Independent Luna review of each remaining `pending` ID and every documented source mismatch | Zero pending IDs, complete platform gates, full locked suite, strict Clippy, rustfmt and signed host tests |

Workers should report concrete changed files, test commands and results, and
unimplemented behavior. Sol integrates only when the ownership boundary is clear
and the option's observable effect is tested. The current increment establishes
the inventory and settings foundation, host and application lifecycle controls,
session renewal, network policy, Steam and Playnite provider slices, a live
input policy, and certificate-bound streaming overrides. It does not pass
the final parity gate.

Each worker commits its owned implementation after rustfmt, meaningful tests,
and Clippy. Use the repository's Nonary Git identity and include these exact
lines in every plan commit, including integration commits:

```text
Orchestrated By: GPT-6 Sol High
Planned By: GPT-6 Astra High
```

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
remote image limit; cached images do not survive restart, and
low-resolution local art is not replaced with converted remote art as in the
reference. Local discovery now ranks bounded portrait candidates by parsed
resolution across configured Steam roots.
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

The `external_ip` text field now persists in product document v12 and has a
signed editor. Omitted updates preserve the value; null or empty resets to
automatic, and nonempty text is retained exactly within the host's bounded
input policy. The pinned Vibeshine source parses and displays this field but
has no runtime reader; its documentation promises automatic detection when
blank. Address detection and advertisement remain pending in Amaterasu, as
does qualification of the source's unbounded text acceptance.

`origin_web_ui_allowed` and `csrf_allowed_origins` remain pending. The first
controls which remote peer networks may reach Vibeshine's web UI; Amaterasu's
management listener currently enforces loopback peers. The second is a
cookie-session CSRF bypass allowlist in the reference, whereas Amaterasu uses
scoped bearer authorization. Its existing CORS `allowed_origins` setting does
not grant authentication and must not be treated as either parity field.

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
for `audio_sink_capture_only=false` and audio endpoint qualification on other
supported platforms remain pending. The other shared override keys and their
runtime consumers remain pending.
Request-supplied client IDs are not used as authority. The editor also
preserves explicit executable lifecycle values and preparation steps.

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
Platform UDP segmentation offload and its compatibility behavior remain
pending.

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
disable and legacy document migration, with a signed editor. Controller,
gamepad, and other platform-specific input effects remain pending.

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
