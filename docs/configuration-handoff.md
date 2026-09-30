# Configuration parity handoff

Reference: Vibeshine `457237db7e57f9c4be21c35bb07e28d1c100eb7d`.
The inventory contains 242 stable IDs (197 global, 41 application, four aliases)
and 103 application/client override keys. All IDs remain `pending` under the
strict end-to-end and supported-platform parity rule.

## Active execution

The 2026-09-30 resume uses six native Luna feature coordinators, with Medium
through Max reasoning matched to worker complexity and at most twelve active
workers. No separate `codex exec` processes are used. The latest user model
selection supersedes the historical Ultra/model tiers in the execution plan.

Main is the sole landing destination. The resumed baseline is `e498571`.
Fresh build-enabled worktrees and isolated Cargo targets are under
`/tmp/amaterasu-20260930-luna`, with a manifest and `team-N` handoff per feature.
Earlier Linux/Windows branches, interrupted worktrees and mailboxes remain
preserved. Read the current override in
[configuration-execution-plan.md](configuration-execution-plan.md) for ownership,
recovery evidence and launch-overlap coordination. Socket-backed qualification
must run outside the restricted network sandbox with an explicit loopback scope.

## Landed behavior

| Assignment | Implementation and local evidence | Remaining qualification |
| --- | --- | --- |
| Scoped input and editor | Five advanced input overrides and ordered keybinding lists persist globally and in application/certificate-bound client settings, with signed editors and frozen launch snapshots. Whole-list client precedence preserves explicit empty replacements. The DS4 Back-button mapping now has certificate-bound sparse client precedence, shipped editor controls, signed authorization checks and restart/launch readback at `cc2bc0f`. Native key plans retain original source ownership across mapping changes and target collisions. The grabbed Linux controller passes as the normal user; the grabbed keyboard mapping/state/teardown test passes with narrowly authorized elevated readback. | Synthetic probes observe requested policy, not effective global defaults or native delivery. Normal-user keyboard readback fails before events because the owned evdev node is not readable. Installed service permissions, desktop cadence, Windows and game/client delivery remain unqualified. |
| Application lifecycle | Bounded PATH lookup and independent detached directories are implemented. Raw undo snapshots max(exit timeout, 15 seconds); provider preparation forwards the same policy. Sequential cleanup budgets retain child ownership and retryable undo. Settled command outcomes and byte-bounded report pagination are tested. The executable elevation editor is present. | Shared append-open output now follows the owned application/preparation/undo lifecycle at `e324a0c`. Outer cleanup budgets and retained deadline failure at `f07abb5` pass signed natural-exit/cancel/RTSP teardown tests with 21-second undo on integrated `e65f70b`. Raw do still has a ten-second deadline, environment expansion occurs at execution, and failed undo is retained rather than forgotten. Host-worker EOF has a separate five-second deadline. Windows SYSTEM/UAC, installed games, quoting/associations and macOS remain unqualified. |
| Software codec | Opt-in system x264 produces real H.264 from NV12 with Annex B output, timestamps, forced IDR, owned backpressure and drain. Four native tests pass with independent FFmpeg decoding. A signed encrypted public stream using real VIDEO/x264 and controlled native NV12 frames also passes independent decoding, later ENet IDR, variable-shard FEC and balanced capture/audio cleanup. | The controlled source does not qualify desktop capture. Default artifacts disable x264; Windows/macOS builds, game/client playback, high-load performance and enabled-artifact distribution remain unqualified. |
| Encoder selection | Legacy ordinals, stable identities and auto/software selectors resolve before capture allocation. Durable nullable global/application/certificate tuning resolves per field and freezes at admission. Signed public synthetic consumers confirm forwarding, old-ticket retention, authorization, teardown and real disabled-backend rejection. Config-only paired-client writes preserve confirmed certificate trust; uncertain or actual trust changes fail closed. | Auto now probes each owned candidate before allocating capture and falls back after rejected probes at `e498571`. The signed Auto-to-x264 consumer passes independent H.264 decoding, later IDR requests and balanced source teardown at `f72fe7a`; the only candidate in that fixture is x264. Unsupported vendor selectors are rejected explicitly; hardware and native platform behavior remain unqualified. |
| Management peer policy | A management-owned CAS document persists exact `pc`/`lan`/`wan` selection and nullable default LAN. Actual peer classification is separate from browser Origin checks. Remote access requires configured TLS; plaintext and operational health stay PC-only. Uncertain writes fail closed until child settlement and confirmed readback. Signed HTTPS tests cover authorization, Host/Origin checks, persistence, restart and signed editor delivery. An opt-in isolated Linux network test checks real PC/LAN/WAN socket peers, TLS trust/identity rejection, narrowing disconnect and PC-only health. | Physical LAN/WAN reachability, installed TLS/service provisioning, Windows/macOS, the reference welcome exception and UPnP behavior remain pending. Generated development certificate SANs remain loopback-only. |
| Management CSRF policy | Management document v2 and its signed editor preserve nullable exact HTTPS exception lists through CAS and restart. Only the cookie CSRF predicate is relaxed; TLS, CORS, Host, actual peer admission, credentials and route scopes remain mandatory. Adversarial tests cover exception revocation, pending requests and uncertain writes. | Reference prefix/Referer/missing-Origin exemptions and minted/query CSRF tokens are not reproduced. Installed service/CA trust and Windows/macOS qualification remain pending; signed Linux host/browser editing and cold-process remembered-session restoration pass at `bc95f60`. |
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

On this Linux host, the combined workspace checkpoint at `5d3b497` passed 2,865
tests, with 24 ignored cases, and strict all-target Clippy, rustfmt and inventory
checks. The frontend passed typecheck, all ten test files and a deterministic
build including the PipeWire chooser. The signed long-cleanup
regression passed against the preceding `e65f70b` snapshot. Its logs and checkpoint
manifest are under `/tmp/amaterasu-20260930-luna`. Increment handoffs
include focused package gates, rustfmt and independent review. Re-run the
checkpoint after later source changes; do not present an older count as
validation of a newer checkout.

The earlier combined frontend passed typecheck, 198 tests and a deterministic
build. The original controlled browser fixture qualified remembered reload,
peer sign-out, HTTPS cookies, CSRF, refresh and logout in Linux Chromium.
At `d2b9bf0`, the signed native HTTPS host and signed production frontend also
qualified remembered login, HttpOnly cookies, CSRF denial, policy edit/readback,
refresh rotation, native host restart, revocation and peer sign-out. At
`bc95f60`, that gate additionally closes Chromium, reopens its same private
profile without the session-only access cookie, and restores login through its
persistent refresh cookie without extending the native refresh deadline. Its
SPKI exception covers only the newly generated test identity; installed trust,
physical remote browsers and other operating systems remain unqualified.

The native cold-Chromium gate passed again at `5d3b497`, including durable
host restart and refresh rotation with a new v3 sidecar. Authentication tests
separately cover legacy v2 context migration before refresh and reload; the
native browser fixture does not combine legacy migration with its host run.

The signed public-launch test passed encrypted RTSP, ENet and UDP media,
certificate-bound scoped input requests, restart persistence, attached natural
exit, cancellation, TEARDOWN and revocation cleanup. Its video/audio sources and
input backend are synthetic. The separate software probe exercises real encoding
and independently decoded H.264 from controlled native NV12 frames. Its public
stream qualifies both Auto and explicit software selection at `f72fe7a`;
each run decoded 13 frames with two IDRs and one balanced capture lease. This
sole-candidate fixture does not qualify fallback after a hardware rejection. Neither
qualifies desktop capture, display restoration or game-visible input delivery.

The signed encoder policy and both real x264 public stream modes passed again
against final combined source `5d3b497`, each requesting `min_threads=3` and
independently decoding 13 H.264 frames with two IDRs and balanced capture leases.

The prior Windows run recorded workspace build/tests/Clippy, frontend and signed
management success at `dd3719d`. Those records are historical evidence; this
session has not established a Windows or macOS native test environment.
Successful provider-owned Playnite override replay remains unqualified without
a completed imported Playnite snapshot.

At `2c12076`, NVHTTP validates RTSP address-family, explicit IP and derived port
readback before activation, and rejects missing policy fields. The signed Linux
network qualification at `528819f` covers HTTP/HTTPS, encrypted RTSP, real IPv4
and IPv6 UDP ownership, deferred rebinding, conflicts and restart persistence.
Four wildcard socket cases additionally pass in disposable user/network
namespaces containing only loopback. These establish family separation,
same-family peer-route selection and IPv6 preflight conflict handling inside
the namespace; physical LAN/WAN access remains unqualified. The signed network
fixture pins its legacy certificate exactly but lacks modern SAN/endpoint
identity qualification.

Scoped software `min_threads` at `6e5c6d8` persists independently across global,
application and certificate-bound client settings and freezes at launch. The
supported request range is 1 through 256, default two; older v19 documents
retain that default. Signed policy tests cover scoped precedence, CAS, restart,
null reset and malformed values. Real x264 parameter reads confirm exact one-
and three-thread behavior on the tested frame; a request of 256 is accepted and
clamped by the codec to seven at 640x480 on this runtime. The configured/effective
policy shown by the host is the resolved request, not live codec allocation.
The reference parser accepts unbounded integers, so exact parser parity remains
pending; these tests also do not establish encoding performance.

The PipeWire provider at `45b2e2b` has 23 passing local tests, reviewed native
buffer/callback ownership, separate selection/capture allowlists, explicit
provider forwarding and a limited LinuxProbe composition. Its opt-in wrapper
proves signed MediaProbe loading, native ABI binding, status and empty
enumeration without opening a picker, then separately reports
`screencast_interface_unavailable` on this user's live bus (exit two). The
management/editor follow-on at `5d3b497` adds dedicated `capture.read`/`capture.write`
routes, explicit monitor/window selection, sanitized responses and bounded
polling/cancellation. Private session sidecar v3 preserves the stable session
context used to scope picker requests across refresh and authentication-session
reload. The operation map is process-local; management/host restart does not
restore an in-flight picker. Legacy v2 contexts migrate, while v1 sessions
without a policy generation are pruned. Every request still requires current
authorization. Video enumerates before opening
PipeWire and rejects stale or incompatible selections. Release tests validate
LinuxProbe grants; the signed MediaProbe proof does not establish an actual
LinuxProbe management-to-portal chooser run. Actual source permission, pixels,
desktop/client playback and load remain gates.

## Continue from the current source

Qualify signed management-to-portal selection when a ScreenCast portal is
available, then validate real desktop capture and client playback. The chooser
implementation and signed MediaProbe no-picker control proof are complete.
Outer launch cleanup budgeting, paired-client DS4
Back-button precedence, scoped software threads, real Auto-to-x264 streams,
unified network policy and cold-browser remembered-session checks have landed.
Their native device, installed service, remote access and complete platform
parity gates remain open.

The earlier Linux run recorded KDE Wayland and black Xwayland root pixels apart
from the cursor. The portal capture library is now implemented in LinuxProbe.
On 2026-09-30, a read-only query found that the current user's portal service does
not expose ScreenCast. Actual source permission, desktop frames and client
playback therefore remain unqualified. Keep useful local work moving while that
gate is unavailable, and keep unimplemented vendor/backend/display/HDR/HEVC/AV1
settings explicit. Older execution notes predate cookie authentication, private
Steam cache-root injection, live Lutris admission checks, scoped input editors,
and native x264; check current source before selecting another slice.

Run rustfmt, meaningful tests, strict all-target Clippy, frontend checks when
changed, and `python scripts/inventory_configuration.py --check`. Default socket
tests remain loopback-only; remote/wildcard tests are separate named opt-in cases.
Build, enumeration, metadata and synthetic protocol proof cannot establish the
actual client/game result. Do not mark an ID verified until its full runtime,
migration, management, editor and supported-platform gates are complete.
