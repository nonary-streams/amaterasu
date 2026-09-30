# Configuration parity handoff

Reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`.
The inventory contains 242 stable IDs (197 global, 41 application, four aliases)
and 103 application/client override keys. All IDs remain `pending` under the
strict end-to-end and supported-platform parity rule.

## Active execution

The 2026-09-29 run uses one Ultra session and its native agents API. Three initial
child threads were reused across six logical assignments. After daemon recovery,
19 concurrent slots support six feature coordinators and up to twelve active
workers, with no more than three per coordinator and no worker delegation.
No separate `codex exec` processes are used. Follow the current override in
[configuration-execution-plan.md](configuration-execution-plan.md), preserving
its independent build directories and continuous reviewed landing policy.

Main is the sole landing destination. Current worktrees are
`/home/chasep/sources/worktrees/amaterasu-20260929-ultra-feature-N_large`;
matching build directories are under `/home/chasep/sources/builds`.
Ready handoffs and test logs are in
`/home/chasep/sources/worktrees/amaterasu-config-runs/20260929-ultra-native`.
Preserve source branches and unfinished work. Inspect the current Git tip and
mailboxes before continuing; historical Windows lanes are not the current run.

## Landed behavior

| Assignment | Implementation and local evidence | Remaining qualification |
| --- | --- | --- |
| Scoped input and editor | Five advanced input overrides and ordered keybinding lists persist globally and in application/certificate-bound client settings, with signed editors and frozen launch snapshots. Whole-list client precedence preserves explicit empty replacements. Native key plans retain original source ownership across mapping changes and target collisions. The grabbed Linux controller passes as the normal user; the grabbed keyboard mapping/state/teardown test passes with narrowly authorized elevated readback. | Synthetic probes observe requested policy, not effective global defaults or native delivery. Normal-user keyboard readback fails before events because the owned evdev node is not readable. Installed service permissions, desktop cadence, Windows and game/client delivery remain unqualified. |
| Application lifecycle | Bounded PATH lookup and independent detached directories are implemented. Raw undo snapshots max(exit timeout, 15 seconds); provider preparation forwards the same policy. Sequential cleanup budgets retain child ownership and retryable undo. Settled command outcomes and byte-bounded report pagination are tested. The executable elevation editor is present. | Shared append-open output remains in progress. Raw do still has a ten-second deadline, environment expansion occurs at execution, and failed undo is retained rather than forgotten. Host-worker EOF has a separate five-second deadline. Windows SYSTEM/UAC, installed games, quoting/associations and macOS remain unqualified. |
| Software codec | Opt-in system x264 produces real H.264 from NV12 with Annex B output, timestamps, forced IDR, owned backpressure and drain. Four native tests pass with independent FFmpeg decoding. A signed encrypted public stream using real VIDEO/x264 and controlled native NV12 frames also passes independent decoding, later ENet IDR, variable-shard FEC and balanced capture/audio cleanup. | The controlled source does not qualify desktop capture. Default artifacts disable x264; Windows/macOS builds, game/client playback, high-load performance and enabled-artifact distribution remain unqualified. |
| Encoder selection | Legacy ordinals, stable identities and auto/software selectors resolve before capture allocation. Durable nullable global/application/certificate tuning resolves per field and freezes at admission. Signed public synthetic consumers confirm forwarding, old-ticket retention, authorization, teardown and real disabled-backend rejection. Config-only paired-client writes preserve confirmed certificate trust; uncertain or actual trust changes fail closed. | Automatic selection currently prefers registered hardware, then available software. Actual candidate probing/fallback is in progress. Unsupported vendor selectors are rejected explicitly; hardware and native platform behavior remain unqualified. |
| Management peer policy | A management-owned CAS document persists exact `pc`/`lan`/`wan` selection and nullable default LAN. Actual peer classification is separate from browser Origin checks. Remote access requires configured TLS; plaintext and operational health stay PC-only. Uncertain writes fail closed until child settlement and confirmed readback. Signed HTTPS tests cover authorization, Host/Origin checks, persistence, restart and signed editor delivery. An opt-in isolated Linux network test checks real PC/LAN/WAN socket peers, TLS trust/identity rejection, narrowing disconnect and PC-only health. | Physical LAN/WAN reachability, installed TLS/service provisioning, interactive signed-host browser editing, Windows/macOS, the reference welcome exception and UPnP behavior remain pending. Generated development certificate SANs remain loopback-only. |
| Management CSRF policy | Management document v2 and its signed editor preserve nullable exact HTTPS exception lists through CAS and restart. Only the cookie CSRF predicate is relaxed; TLS, CORS, Host, actual peer admission, credentials and route scopes remain mandatory. Adversarial tests cover exception revocation, pending requests and uncertain writes. | Reference prefix/Referer/missing-Origin exemptions and minted/query CSRF tokens are not reproduced. Installed-host browser and Windows/macOS qualification remain pending. |
| Migration and Linux providers | Contained migration aliases and deterministic inventory are implemented. Signed controlled current-user Lutris and Steam tests cover import, restart, canonical launch metadata, owned process tracking, stop, abort recovery and lease reacquisition. Steam stop preserves original clients, preexisting game identities and unrelated processes; unavailable install-tree evidence leaves liveness unknown. | Installed-host migration and the complete platform matrix remain pending. Controlled native process fixtures do not qualify actual Steam games, Lutris/Wine/Flatpak, desktop integration, cross-user discovery or Gamescope capture handoff. |

Unix NVHTTP and RTSP listeners now allow immediate restart after TIME_WAIT while
still rejecting an active competing listener. Catalog launch provenance uses
actual revision-bound storage cursors and is tested across multiple pages;
synthetic prefix cursors are no longer accepted. Authenticated management now
admits the existing nullable LAN/WAN encryption modes as exact integers 0..2.

The signed public stream fixture explicitly enables input, uses free loopback
base/derived protocol ports, and configures mandatory LAN encryption through the
real authenticated settings route before restart. Controlled applications set
`auto_detach:false`: the reference-compatible default retains a successful root
that exits within five seconds, which would test a different lifecycle.

## Validation evidence

On this Linux host, the combined workspace checkpoint at `569b389` passed 2,753
tests, with 22 ignored cases, and strict all-target Clippy, rustfmt and inventory
checks. Increment handoffs
include focused package gates, rustfmt and independent review. Re-run the
checkpoint after later source changes; do not present an older count as
validation of a newer checkout.

The combined frontend passed typecheck, 198 tests and a deterministic build.
Both opt-in browser session qualifications also passed in actual
Linux Chromium: remembered-session reload and peer sign-out, and HTTPS HttpOnly
cookies, CSRF, refresh and logout. Their server is a controlled fixture; signed
management tests separately exercise the real authentication service.

The signed public-launch test passed encrypted RTSP, ENet and UDP media,
certificate-bound scoped input requests, restart persistence, attached natural
exit, cancellation, TEARDOWN and revocation cleanup. Its video/audio sources and
input backend are synthetic. The separate software probe exercises real encoding
and independently decoded H.264 from controlled native NV12 frames. Neither
qualifies desktop capture, display restoration or game-visible input delivery.

The prior Windows run recorded workspace build/tests/Clippy, frontend and signed
management success at `dd3719d`. Those records are historical evidence; this
session has not established a Windows or macOS native test environment.
Successful provider-owned Playnite override replay remains unqualified without
a completed imported Playnite snapshot.

## Continue from the current source

Continue append-open application output, owned automatic encoder probes, public
listener policy and signed-host Chromium session qualification. The current
desktop is KDE Wayland; Xwayland root reads show black pixels apart from the
cursor. A portal-selected PipeWire capture provider is in progress, with actual
source permission an explicit native acceptance gate. Continue useful local
behavior while native platform gates are unavailable. Keep every
unimplemented vendor/backend/display/HDR/HEVC/AV1 setting explicit. Check current
source before choosing another slice: older execution notes predate cookie
authentication, private Steam cache-root injection, live Lutris admission checks,
scoped input editors and the native x264 backend.

Run rustfmt, meaningful tests, strict all-target Clippy, frontend checks when
changed, and `python scripts/inventory_configuration.py --check`. Default socket
tests remain loopback-only; remote/wildcard tests are separate named opt-in cases.
Build, enumeration, metadata and synthetic protocol proof cannot establish the
actual client/game result. Do not mark an ID verified until its full runtime,
migration, management, editor and supported-platform gates are complete.
