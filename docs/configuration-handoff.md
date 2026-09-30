# Configuration parity handoff

Reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`.
The inventory contains 242 stable IDs (197 global, 41 application, four aliases)
and 103 application/client override keys. All IDs remain `pending` under the
strict end-to-end and supported-platform parity rule.

## Active execution

The 2026-09-29 run uses one Ultra session and its native agents API. The API
allows three child threads; they are reused across six logical assignments.
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
| Scoped input and editor | Five advanced input overrides persist in catalog and certificate-bound client records, validate through management, and have application/client editors. Signed API tests cover false, zero, invalid types, CAS and restart. Signed HTTPS launch exercises client precedence, application fallback, forwarded null inheritance, and input-probe lease cleanup. A grabbed native Linux controller test passes as the normal user, checking exact events, kernel state, neutralization and device removal. | The probe observes requested policy, not native delivery or effective global defaults. Native qualification covers the test user's owned controller, not an installed service account or a game. Windows input, game-visible behavior and keybindings remain pending. |
| Application lifecycle | Bare executable PATH lookup includes relative and empty entries with bounded search. Raw preparation and detached targets resolve before directory inference. Detached children infer their own directory when the application directory was omitted; explicit and typed child directories keep precedence. Real controlled-process tests cover directory selection and retry conflicts. The executable elevation editor is present. | Windows SYSTEM/UAC, installed-game behavior, platform quoting/associations and reference preparation timeout/output/failure semantics remain pending. Raw environment expansion timing still differs from reference configuration parsing. |
| Software codec | Opt-in `amaterasu-encode/software-x264` performs actual native x264 H.264 encoding with NV12 input, Annex B output, timestamps, forced IDR, owned backpressure and drain. Four native tests passed with independent FFmpeg decoding. Default and missing-header builds report unavailability. | Default artifacts do not enable x264. Windows/macOS builds, live capture/client playback, performance and an intentional enabled-artifact distribution choice remain pending. |
| Encoder selection | Legacy ordinals, stable identities and `auto`/`software` selectors resolve once before capture allocation; unavailable selection allocates no capture. Durable nullable global/application/certificate-bound client tuning resolves per field and freezes at admission. Signed real-module tests cover authorization, CAS, null versus empty, restart and genuine disabled-feature availability; editor tests preserve literal strings. Cancellation and uncertain admission retain ownership until settlement. | Signed public-stream projection is in progress. Automatic selection currently prefers registered hardware, then available software; failed-hardware probing/fallback remains pending. Unsupported vendor selections are rejected explicitly. |
| Management peer policy | A management-owned CAS document persists exact `pc`/`lan`/`wan` selection and nullable default LAN. Actual peer classification is separate from browser Origin checks. Remote access requires configured TLS; plaintext and operational health stay PC-only. Uncertain writes fail closed until child settlement and confirmed readback. The signed HTTPS host test covers authorization, Host/Origin checks, persistence, restart and signed editor delivery. | Actual remote peer admission, narrowing disconnect, certificate identity/trust, interactive signed-host browser editing, Windows/macOS, the reference welcome exception and UPnP behavior remain pending. Generated development certificate SANs remain loopback-only. |
| Migration and Linux gates | Contained legacy aliases are read through validated physical targets while preserving logical file classification. Inventory generation is deterministic. Linux workspace builds and tests were repaired without changing Windows-only field expectations or unsupported cleanup behavior. Signed current-user Lutris tests cover controlled import, durable disable/restart, canonical URI argv, owned stop, pidfd abort cleanup, exclusive lease reacquisition and missing-client rejection despite an available system fallback. | Installed-host migration and the complete native platform matrix remain pending. Controlled fixtures do not qualify actual Lutris/Wine/Flatpak delivery, desktop integration or cross-user discovery. Signed controlled Steam tracking qualification is in progress. |

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

On this Linux host, the combined workspace checkpoint at `b988ebc` passed 2,666
tests, with 21 ignored cases, and strict all-target Clippy. Increment handoffs
include focused package gates, rustfmt and independent review. Re-run the
checkpoint after later source changes; do not present an older count as
validation of a newer checkout.

The combined frontend passed typecheck, 189 tests and a deterministic build.
Both opt-in browser session qualifications also passed in actual
Linux Chromium: remembered-session reload and peer sign-out, and HTTPS HttpOnly
cookies, CSRF, refresh and logout. Their server is a controlled fixture; signed
management tests separately exercise the real authentication service.

The signed public-launch test passed encrypted RTSP, ENet and UDP media,
certificate-bound scoped input requests, restart persistence, attached natural
exit, cancellation, TEARDOWN and revocation cleanup. Its video/audio sources and
input backend are synthetic. It does not qualify native capture, decoded live
H.264, display restoration or game-visible controller delivery.

The prior Windows run recorded workspace build/tests/Clippy, frontend and signed
management success at `dd3719d`. Those records are historical evidence; this
session has not established a Windows or macOS native test environment.
Successful provider-owned Playnite override replay remains unqualified without
a completed imported Playnite snapshot.

## Continue from the current source

Continue scoped keybindings, signed software-stream projection and controlled
Steam tracking, landing each reviewed increment promptly. Continue useful local
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
