# Provider failure boundaries

The twenty warnings in the initial core audit are two warnings for each of ten
provider symlinks: skipped symlink and unavailable module source. The sources
were present. Review used their resolved, clean Git identities and a bounded
source-only projection with file hashes, reusing the rejection-audit tool
unchanged. A projection scan is not a whole-host scan or a runtime proof.

| Provider | Failure/control boundary reviewed | Result |
| --- | --- | --- |
| audio | `src/worker.rs::run`, `src/controller.rs::read/close` | Device loss reopens the same selected endpoint/format with bounded retry. Terminal format/endpoint faults return native status while queued PCM and ownership remain retained. Explicit owner close and unpublished-open rollback release capture; neither is a recoverable network rejection. |
| audio-encode | `src/stream.rs::fail`, `src/controller.rs::recover/close` | Codec faults retain prior packets and support a new recovery epoch. Failed bitrate updates preserve the old configuration. Native failure status is a report for the owner; it does not close the RTSP ticket. |
| capture-vkms | `capture/src/presenter.rs::failed`, `capture/src/frames.rs::set_fault` | Busy/interrupted helper replies retry; pool exhaustion skips a presentation. Output loss, helper death, snapshot failure and permission loss remain genuine source faults. Frame leases, descriptor ownership and helper authorization cannot be inferred from an advisory report. |
| display | `src/pipeline/stream_issues.rs::display_issue`, scoped/journal transactions | Compatible unchanged-display/profile fallbacks are reported as demotions. Uncertain mutations retain journals/recovery leases. A diagnostic fallback does not establish that an explicitly required output/mode was admitted or that rollback retired native ownership. |
| encode-video | `video/src/pipeline.rs::cleanup/fail_retirement/failed_step` | Fixed: typed reports reach the common SDK decision before active video cleanup or failed retirement state. Warnings cannot stop video or create cleanup barriers. Owner close/drain/shutdown are explicit controls. Uncertain parent liveness retries without inventing cancellation; lost native completion/authorization remains critical. |
| frame-limiter | `src/lifecycle.rs::decide_with_budget`, owner `coordinator/providers.rs` | Known unavailable read-only planning backends use the no-op plan. An optional participant's refused plan is skipped only before any durable effect; callbacks and uncertain worker/driver restoration retain ownership fences. The provider reports results to the owner and does not own an RTSP ticket. |
| input | `src/controller.rs::expire/finish_lease`, `src/stream_issues.rs::batch_reply` | Optional controller/feedback/batch failures drop only their events. Lease expiry, permission/policy revocation, failed release of held input and cleanup-required state must remain fail-closed at the input authority. They cannot be overridden by a stream-continuation warning. Keyboard-owner source is unchanged. |
| remote-session | `src/monitor/mod.rs::reject_status`, source/reference retirement | Invalid observations withdraw unverified readiness and retry; retained references/captures are not declared retired. Failed mode changes preserve the old source, and another authenticated reference prevents native source destruction. Final stream admission/termination belongs to the owning consumer. |
| rtx-hdr | `src/lifecycle.rs::respond`, `src/display_adjustment.rs`, GPU live settings | SDR bypass is a no-op demotion. Rejected live updates preserve the admitted processor. Explicit selected HDR/display mutations require exact scope/lease and rollback proof; unavailable source preparation cannot silently substitute a different required media contract. |
| stats/history | telemetry/history store and NVHTTP history reservation | Storage/sample/event errors already continue independently. Fixed: uncertain history reservation now retains its exact call in bounded background settlement and continues launch without history. Reservation admission is skipped when all 64 settlement slots are held; late completion is settled before releasing that exact activity. Native application/RTSP mutation uncertainty remains critical. |

Provider-native failures are facts about local resources. A worker cannot emit
valid PCM after its codec fails, retain authorization after lease expiry, or
claim a lost display/DRM lease is still owned. The shared SDK gates the migrated
stream failure effects; it does not grant authority to fabricate healthy native
resources or weaken provider security checks.

A native plugin remains trusted machine code. It can close handles, return false
status, exit, deadlock, crash or deliberately bypass Rust APIs. API grants and
`StreamTermination` are not a sandbox. Enforcing against hostile plugins requires
an isolated process/resource authority and versioned delivery contracts, not
another copy of rejection policy in each layer. No universal enforcement claim
is made. Rust AST coverage also does not validate C/C++, drivers, GPU execution,
feature-specific platform behavior or a deployed replacement plugin.

Validation covers the SDK, three stream owners and all ten provider repositories,
including the independent VKMS capture/helper workspace. Installed privileged
hardware tests remain opt-in. No service/package activation or live playback is
performed by source/unit validation.


The independent rejection review for policy commit `1c59be5e` was also checked
against this continuation. Its 26 curated entries include 23 source-anchored
failure/recovery cases, one expected stop and two incomplete anchors. S05's old
overflow-503 anchor is absent because bounded request backpressure replaced it;
S20's limiter source is available in the resolved-provider projection. S11 led
to a further fix: an ordinary I/O loss of a queued RTSP request that never
mutated native state now preserves its ticket, while invalid framing and
uncertain native completion remain critical. Parked native jobs participate in
that ownership check. The history-reservation case is additional to catalog
S19, which covers release. The review's 65 uncertain cause groups remain
candidates, not 65 proven violations or proof that other paths are safe.
