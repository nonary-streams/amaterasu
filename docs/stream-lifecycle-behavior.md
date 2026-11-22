# Stream lifecycle ownership and recovery

Amaterasu treats transport, application, capture, input, audio, display and
preparation as separate lifetimes. A successful request submission, expired
request, changed host epoch or empty local list does not prove a native effect
has retired. Consumers use the selected versioned provider contracts and retain
unresolved ownership until those contracts establish retirement.

Retaining ownership is not the same as holding admission. A record keeps its
cleanup identity and what it still owes for as long as it exists, and reports
that honestly. It stops counting against any limit, slot or lock the moment
its owner is asked to end. `lifecycle-admission-and-retirement.md` states that
rule and where each layer applies it.

## Shutdown order

A consumer closes admission, fences startup, and drains owned work while its
providers continue receiving ticks. Trusted package dependencies determine when
a provider may begin its own quiescence. An unrelated failing component must
not suppress progress in an independent branch. Each shutdown budget remains
bounded; polling must not extend a cleanup deadline or automatically replay an
arbitrary failed undo command.

Before consumer drain, the host announces its remaining budget through the
optional versioned `ModuleShutdownApi`. The SDK exposes that same contract to
external modules as `ManagedModule::prepare_shutdown`. Announcement closes new
admission and stores the earliest deadline; it does not begin provider teardown.
Application stop, active and future undo commands, and destructor fallback share
that deadline. The standard limits leave 20 seconds for module drain, 25 seconds
for worker exit and 30 seconds before the service manager intervenes. Legacy
modules without the optional announcement retain their older contract; the host
cannot infer that their destructors implement the deadline.

Direct SDK callers need the same shutdown guarantees as Moonlight callers.
Launch, RTSP and stream-session therefore own cooperative cleanup themselves;
NVHTTP is not the sole owner of their shutdown progress. Destructors are a final
bounded fallback and cannot substitute for cooperative provider progress.

A cancellation before submission must not submit a new effect merely to cancel
it. After submission becomes uncertain, cleanup retains the exact operation and
request identity, settles that request, and obtains authoritative lookup or
retirement evidence. Cancelling a hook also cancels its delegated request tree
before the hook's participant request is released.

## Stream and application lifetimes

An ordinary disconnect releases the departing transport and its input. A
resumable application retains its application policy and any shared source
still owned by another consumer. An explicit application end drains all related
transports before application cleanup; a configured stream-end lifetime policy
can request that same end behavior. A joined transport never acquires the
application owner's cleanup authority.

Quit can arrive after disconnect has already started transport cleanup. It
upgrades that cleanup to end the application within the existing deadline;
late transport status replies cannot downgrade Quit back to pause.

Capture and input retirement precede topology restoration. Pending display
removal remains pending until the provider observes disappearance. A safely
parked display may release active stream admission while retaining its durable
native recovery record and reporting the actual parked output.
The same removal guard applies when an intermediate setup layout fails to
restore. On Linux an enabled output published by KScreen and Wayland must also
match an unambiguous, connected, enabled and powered-on physical DRM connector.
The provider retains any unconfirmed snapshots for recovery.

### Linux physical-output handoff review

Codex reviewed the Linux virtual-display worker, KScreen/Wayland observation,
DRM connector check and parked-output reporting. On disconnect or Quit, RTSP
drains the stream through stream-session. The virtual-display worker attempts
desktop restoration and verifies the physical replacement immediately before
calling the connector broker. Disconnect, timeout and uncertain readback keep
the existing cleanup ownership. A safely parked connector releases its stream
reference with a bounded lifecycle warning, and the provider retries using the
saved desktop state. The RTSP reply still derives from the stream retirement
proof, and media negotiation does not depend on the display guard.

The Linux virtual-display suite exercises powered-off and ambiguous physical
connectors, virtual peers, delayed readback, parking, later recovery and
restart ownership. The 175 enabled tests and Clippy passed. Physical scanout
and Moonlight playback require live verification.

### Linux game display pause and resume review

Codex reviewed the NVHTTP Open/Quit workflow, the RTSP prepare payload, and
stream-session's display reference and cleanup journal. NVHTTP supplies the
paused display policy for resumable games using an owned Linux display with
restore on disconnect disabled. RTSP binds it to the admitted application
session and authenticated Open caller. An authenticated disconnect drains the
original capture, input and transport. Stream-session reduces the journal to
the display reference before confirming the old transport's retirement.

Resume acquires a distinct reference on the same display when the application,
geometry, refresh and source dynamic range match. It starts fresh capture and
keeps the old reference until the new transport is active. A changed mode or
an explicit provider refusal before any effect retires the old hold before
allocating a compatible display. This wait shares the original startup budget.
An uncertain provider reply retains the exact reference for normal cleanup.
The negotiated client media format and PLAY admission checks apply to either
source. A fallback warning is published once the replacement display is ready.

Quit fences the exact application, drains its transports, verifies the
application stopped, and confirms release of the paused display before
completing. Only the authenticated application principal can request release.
An unconfirmed release is a cleanup warning and yields incomplete
Quit. Inactivity expiry releases just the display. Restart recovers the exact
display reference without replaying the retired transport. Regression tests
cover reconnect, failed reconnect and retry, changed timing, provider refusal,
expiry, restart cleanup and stale Quit replies. This change spans the NVHTTP,
RTSP and stream-session components, which ship together. Real Moonlight
playback and physical scanout require live verification.

### PyroWave 4:4:4 negotiation review

Codex reviewed the PyroWave 4:4:4 encoder revision and its Core gitlink,
RTSP and stream-session changes, and signed composition update based on
`b129e39f`. Serverinfo advertises the 4:4:4 bits when the composition enables
both new launch profiles. Stream-session probes the selected launch depth and
range in both 4:2:0 and 4:4:4 before RTSP publishes Moonlight's shared PyroWave
marker. Each probe uses the exact encoder, media profile, and framing chain.
The read-only probes share the original launch budget. A failed, malformed,
or unconfirmed probe discards the partial offer and records a lifecycle warning.
A pending read still settles against its original operation and cannot add a
profile to a completed offer. RTSP proceeds with the standard codec offer.

ANNOUNCE must match a verified profile. PLAY passes its exact sampling and
negotiated color through the existing authenticated startup to video and the
encoder's native configuration. The existing lifecycle owner still decides
startup readiness and owns rollback and retirement. Encryption, source
ownership, packet framing, and cleanup deadlines follow the same contracts.

Coverage includes serverinfo capability bits, legacy and multiple-profile
DESCRIBE replies, Moonlight-shaped SDR and HDR 4:4:4 ANNOUNCE, the selected
video Open, optional probe rejection and timeout with standard startup, and
GPU encode-decode at 8-bit SDR, 10-bit SDR, and HDR10. Live client playback and
installed-package activation require separate verification.

### Extension link-budget ownership review

Codex reviewed the framer ABI v2.2 change against Core `bacbc238`, including
the PyroWave encoder submodule, the signed extension route settings, and the
RTSP and stream-session startup paths. RTSP still interprets Moonlight's
PyroWave ANNOUNCE fields. The selected transport supplies framing limits
under the already prepared stream's transport and encryption policy.
The selected framer supplies the encoder payload rate under those limits.

Routes that require link budgeting probe for support without a client bitrate.
A missing v2.2 table or refused profile leaves the standard offer available
and records an extension fallback on that launch. Once an authenticated
ANNOUNCE selects an extension and a wire budget, a refused calculation leaves
the prepared session intact and reports the exact request failure to the
lifecycle owner. The host cannot substitute a different codec or cipher
after the client has selected them. The budget callback is synchronous,
bounded and read-only, with no delayed effect or cleanup ownership.
Accepted bitrate and wire budget are pinned for replay and teardown.

Codex reviewed the extension offer and startup changes against Core `c6126205`.
Stream Session reports the prepared route's wire-budget support with its exact
profile offer after every offered profile passes preflight. RTSP freezes the
support result for the ticket and uses it in both ANNOUNCE validation and
PLAY. A malformed support declaration withholds the extension and records
`invalid_extension_offer` on that operation. Standard negotiation and startup
remain available. An older preparation reply without the declaration retains
PyroWave's established wire interpretation. The selected framer still owns
encoder-rate calculation using the exact prepared limits, and the existing
lifecycle owner handles its refusal after ANNOUNCE. Stream Session rejects a
Start carrying a wire budget unless its prepared route proved support. Older
RTSP versions can still use this path when the signed route requires that
proof, and can use the legacy encoder rate without a wire budget.

The native source's probe and Start omit legacy software thread and QP controls.
The standard Start still carries those controls. The crop, range, chroma,
encryption, source identity and session ownership checks still apply. Regression
coverage exercises a replacement codec's prepared wire budget, stock startup
after an invalid offer, and the existing PyroWave link-rate limit and legacy
bitrate behavior.

Stream-session checks the selected transport's reported overhead against
generic frame-plan bounds and the agreed encryption state. The GameStream
transport remains responsible for checking its exact protocol overhead when
it prepares a frame plan. Constant-rate capture requests pass through RTSP
to the video provider's normal capability check.

### Streaming provider substitution review

Codex and the independent architecture reviewer traced the media, control,
encoder, capture and router changes against Core `bacbc238` and encode-video
`0687cfce`. NVHTTP and RTSP retain request and wire validation. Stream-session
opens the selected media transport under the original prepare operation. The
provider validates packetization before binding, and the session pins its
returned audio wire size. A malformed successful reply leaves the allocated
provider handle in the original cleanup record. A refused required media format
is reported through the stream lifecycle owner with the transport's bounded
reason. Retry and status checks use the pinned result.

Before opening the selected control transport, stream-session verifies that its
v1.1 semantic interface is available. Missing required control support fails
preparation with `control_codec_unavailable` and retires resources already
opened under that attempt. The data plane preserves authenticated record
metadata for the selected decoder. Unsupported optional feedback records a
warning while media continues. The provider retains responsibility for wire
encoding, sequencing and delivery. Control and media replacements ship with
the updated session consumer in the signed update path.

NVHTTP reads encoder capabilities from video, which obtains them from the
configured encoder used for ordinary Open. Enumeration is idle-only, and
failed refreshes keep the prior successful snapshot. Capture Open must confirm
requested constant-rate behavior. Video retains a returned capture handle
before checking capability so refusal can close the correct resource. If the
selected fitter is unavailable and direct submission succeeds, video reports
the fallback. Incompatible frame geometry still needs a fitting provider.
The fitter owns its pending output and fences late completion when the video
stream is retired.

NVHTTP sends only current listener plans to the selected router provider.
UPnP failures affect mapping status independently of stream admission. Workers
retain mapping and pinhole ownership through shutdown. Unchanged plans still
poll their updated status. The scoped PyroWave updater carries the required
provider artifacts and preserves the installed FFmpeg fitting backend when it
was enabled. WebRTC's selected Network SDK binding owns the session UDP handle.

Focused provider replacement tests cover media admission and wire sizing,
control decode and feedback, encoder opening and launch limits, frame fitting,
constant-rate capture retirement, router mapping, and WebRTC UDP retirement.
The signed stream-session and encrypted RTSP probes passed on private loopback
with synthetic capture, real Opus, encrypted UDP, and an independent ENet
client. RTSP exercised authorized destination discovery, pre-PLAY warmup,
and confirmed TEARDOWN. Chromium loopback separately verified WebRTC video
decode, audio delivery and managed session close. Hardware capture, physical
router changes and installed-package activation require separate verification.

### GameStream Opus surround layout review

Codex traced the selected Opus layout from Stream Session's Audio Encoder Open
through RTSP preparation, DESCRIBE, SETUP, ANNOUNCE and authenticated start. The
coordinator records the encoder handle before validating the decoder descriptor
and pins the approved layout to that prepared session. RTSP validates the
descriptor against the ticket's actual channel count before registering the
ticket. The client still has to announce the prepared channels, speaker mask
and packet duration. The audio transport keeps the encoder's effective packet
size and its own wire framing.

Missing or malformed surround metadata leaves no verified decoder configuration
for the requested channels. Stream Session reports that condition to the
lifecycle owner and closes the opened encoder. A malformed prepared response at
RTSP follows its owned session cleanup and never registers the ticket. Those
paths use the existing operation identity, deadline and settlement rules. A late
prepare after cancellation retains ownership for cleanup. Recovery of an open
encoder preserves the decoder layout as required by the provider contract.

The independent Codex reviewer checked the SDP digit order against Moonlight's
parser and identified the need to make same-handle recovery stability explicit.
Focused source fixtures exercise a replacement encoder's non-stock mapping,
malformed metadata retirement, and the mapping recovered by the Moonlight SDP
ordering rules. A standalone harness around the pinned Moonlight parser
confirmed the decoder layouts for the same 5.1 and 7.1 SDP attributes.
Hardware/client playback is outside these fixtures.

## Preparation and restart

Reached preparation steps own their undo commands, including steps with an
empty `do` and nonempty `undo`. Unreached steps own no undo. Cleanup visits owed
undo commands in reverse order, preserving failed or uncertain effects for an
explicit retry.

A lifecycle preparation that finishes after disconnect still belongs to its
original operation. RTSP resolves and validates that preparation; stream-session
persists its cleanup identity before fencing or retirement, without starting
native media merely to stop it.

Restart recovery uses the original caller, target, generation, frozen provider
graph and provider evidence. Releasing one reference cannot remove a resource
still leased by another owner. Persisted virtual-display references are not
retired merely because the manager restarted; global topology restoration must
wait for the recovered reference cohort to release.

Audio absence proof is scoped to the original Pulse server instance and exact
native module. A missing sink alone does not prove an uncertain unload completed.
New identity-bearing intents require a matching server identity, complete native
module observation and an unchanged durable intent before absence can retire
ownership. Legacy intents without that evidence remain conservative. Older
strict readers cannot consume a pending identity-bearing intent; an empty sink
map remains compatible.

## Behavioral reference and regression coverage

The comparison uses Vibeshine commit
`43cf27252a205221a295126a5fcd9ad87ba9c914` as a behavioral reference. Its source is
not copied or translated. The useful invariants are ownership and ordering;
Vibeshine's best-effort continuation after an undo error is not native retirement
proof for Amaterasu.

| Behavior | Vibeshine reference | Amaterasu regression location |
| --- | --- | --- |
| Roll back owned launch work after cancellation | `src/process.cpp`, `proc_t::terminate`; `src/nvhttp.cpp` launch rollback | `modules/launch/src/pipeline/tests.rs` and `tests/shutdown.rs` |
| Drain startup completion before dropping RTSP work | `src/rtsp.cpp`, startup-pool shutdown | `modules/rtsp/src/controller/ownership_tests.rs` and `tests.rs` |
| Preserve application policy on pause and shared-client detach | `src/stream.cpp`, session owner accounting and join; frame-limiter suspend | `modules/nvhttp/src/launch/lifecycle_tests.rs` and `shared_session_tests.rs` |
| Reset input before media drain | `src/stream.cpp`, request_stop and join | `modules/stream-session/src/pipeline/tests/input.rs` |
| Keep a shared native source until its last consumer releases | `src/stream.cpp`, shared runtime ownership | `modules/stream-session/src/pipeline/tests/shared_capture.rs`; lifecycle coordinator regression tests |
| Wait for native display retirement or preserve parked ownership | Linux private-display recovery and virtual-display cleanup policy | `modules/stream-session/src/pipeline/tests/virtual_display.rs`; virtual-display worker tests |
| Run reached undo hooks in reverse order | `src/process.cpp`, launch commands and termination | `modules/application/src/lifecycle.rs` tests and `tests.rs` |
| Continue independent shutdown progress while preserving dependencies | `src/main.cpp`, `src/rtsp.cpp`, application termination | `crates/host/src/loading/teardown_tests.rs` |
| Request graceful worker stop before forced containment | `tools/sunshinesvc.cpp`, user-session worker termination | `crates/host/src/worker_shutdown.rs`; Windows supervisor source review |
| Reconcile exact native resources after restart | Linux private-display recovery; Windows startup display recovery | Stream-session restart tests, lifecycle recovery tests, audio journal tests and virtual-display worker tests |

## Remaining recovery boundaries

Launch currently advertises `durable_recovery: false`. A new launch runtime
cannot certify that a previous runtime's application process and preparation
undo commands retired. The fixed retirement SDK deliberately reports this as
unconfirmed; epoch fencing is not a replacement for durable native recovery.
Historical session cleanup can proceed only when independently verified
application retirement and the required exact child retirement evidence exist.
Unretired managed capture and input from an older runtime require their own
supported native recovery evidence.

The Windows interactive supervisor now requests EOF-driven worker cleanup and
requires successful retirement before replacement. Timeout, failed cleanup and
nonzero worker exit remain errors; forced termination is final containment.
Windows compilation, SCM timing and native crash/restart qualification remain
separate from portable deterministic tests. Linux fixture success does not
establish Windows process, desktop, audio or real-client playback behavior.
None of these source comparisons authorizes editing recovery journals or
interrupting an active stream.
