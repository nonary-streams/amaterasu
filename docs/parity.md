# Vibeshine parity ledger

The ordered application-preparation increment was completed on 2026-09-19; see
the [implementation checkpoint](implementation-checkpoint.md). This advances the
foundation but does not claim full application or streaming-host parity.

Reference: sibling checkout commit `aa292c4cb097f22beab19d6ebf0307367f9bf449`.
This initial ledger names the known feature domains and acceptance gates. The
configuration inventory below is pinned separately; a full route-by-route behavior
inventory remains necessary. No unlisted feature is implicitly declared complete
or out of scope.

## Configuration inventory and current progress

The pinned [configuration inventory](configuration-parity.md) uses Vibeshine
`vibe-test` commit `457237db7e57f9c4be21c35bb07e28d1c100eb7d`. It records
197 global settings, 41 application fields, 103 shared application/client override
keys, and four aliases. [Coverage status](configuration-coverage.json) remains
pending for every reference ID until its full behavior is verified.
The [execution plan](configuration-execution-plan.md) assigns bounded domains
and defines the evidence needed to close each ID.

The configuration service now validates decimal, enum, list, and object fields
and persists separate caller-owned product documents with versioned CAS updates.
Management can edit the NVHTTP host name, its HTTP/HTTPS listener port pair,
and the Steam discovery toggle through fixed, scope-checked routes. The signed
HTTP regression verifies durable updates,
conflict handling, live Moonlight server-info naming (including the PC-name
fallback), listener rebinding, Steam discovery gating, and settings after
restart. Legacy host-name consumers, exact validation, RTSP/UDP port
propagation, and Steam catalog sync, artwork, and launch behavior still need
parity work. The inventory statuses stay pending accordingly.
The application catalog accepts Vibeshine's `working-dir` spelling and applies
its directory precedence to launch, preparation and undo commands. Its
`auto-detach` policy retains a quick successful launcher for explicit cleanup.
Those IDs remain pending because Vibeshine also accepts shell/PATH command
forms and further process-group behavior that Amaterasu does not represent.

The pinned [route declaration inventory](reference-routes.json) and
[Windows regression gates](windows-regression-gates.md) make part of that audit
concrete. Route declarations do not establish compatible payloads or behavior.

**Implemented foundation** means executable code with automated tests, not
feature parity with a deployed streaming host. **Pending** means not implemented.

| Area | Current status | Required acceptance gate |
| --- | --- | --- |
| Minimal native shell | Implemented supervised development serve/probe | Installed service entry, production bootstrap, signal shutdown and unattended policy |
| Independent module loading | Implemented foundation | Real Windows/Linux product modules through a frozen ABI |
| Request/event/hook runtime | Implemented foundation and separately granted native interface binding | Domain schema registry, interface adapters, production quotas and workload tests |
| Startup module settings | Implemented foundation | Secure installed policy provisioning and settings migration |
| Persistent state | Implemented KV and atomic sharded collections | Production recovery policy and migration of existing host data |
| Configuration service | Implemented declarative migrations and post-commit events | Register all product schemas and frontend controls; production schema upgrade policies |
| Session coordination | Implemented paired application/RTSP launch and managed media lifecycle coordination | Display transactions/restoration, reconnect/resume, durable ownership and full client sessions |
| Resource leases | Implemented foundation | Real display/audio/performance restoration and multi-client hardware tests |
| Package integrity and local activation | Implemented foundation | Online update manager, production root trust, key rotation/revocation and catalog |
| Unsigned module mode | Development-only | Administrator-only installed toggle, restart enforcement and visible frontend status |
| Historical component selection | Implemented local mechanism | Authenticated historical catalog, dependency ranges and compatible data snapshots |
| Authentication and permissions | Implemented credentials, session/token revocation and route scopes | Production recovery/provisioning, remote TLS, session refresh and multi-user policy |
| Pairing and host identity | Implemented Gen7 HTTP/TLS pairing, durable identity and native TLS signing | Existing clients pair; imported host identity/pairings remain usable |
| Moonlight-compatible streaming | HTTP/TLS pairing, application lists and configured public launch/cancel, encrypted RTSP, UDP discovery, media/control delivery and synthetic-tested session coordination implemented | Display optimization, complete media negotiation, input/feedback routing, reconnect, resume and real client playback |
| Media UDP transport | Implemented separate native provider, audio/video FEC, media encryption and ticket-token destination discovery | Session scheduling, pacing, control feedback, network qualification and client playback |
| Reliable control transport | Implemented Rust ENet, authenticated control, paired launch/revocation and leased keyboard/mouse dispatch | Controller/feedback routing, congestion integration and real client sessions |
| RTSP negotiation | Implemented encrypted TCP tickets, UDP discovery, armed startup and paired launch/revocation integration | Complete profile negotiation, reconnect/resume and real client qualification |
| Browser streaming | Pending | WebRTC signaling, media, input, reconnect and mixed-protocol ownership |
| WGC/DXGI capture | Implemented Windows DXGI and WGC CPU readback plus persistent global output selection for new streams; hardware unqualified | Desktop transitions, hotplug recovery, fallback, hybrid GPUs and pacing |
| Linux capture | Pending | Supported KDE Wayland paths, PipeWire/DRM and pre-login constraints |
| Video encoders | Implemented Windows hardware Media Foundation H.264; hardware unqualified | NVIDIA/AMD/Intel, codec negotiation/probing and adapter-specific capabilities |
| Video processing and HDR | Implemented BGRA-to-NV12 conversion, provider selection, force-IDR and graceful video drain; HDR pending | Color conversion, HDR metadata, processing, TrueHDR and SDR fallback |
| Physical display management | Implemented Windows classic CCD transactions; hardware unqualified | Resolution, refresh, HDR, scaling, rotation and exact topology restoration |
| Virtual-display adapters | Implemented Sunshine owned temporary-monitor adapter; driver unqualified | Driver protocols, persistent identity, readiness, fallback and cleanup |
| Remote Monitor | Pending | Per-client retained monitors, isolated capture, layouts and peer-safe teardown |
| Display recovery | Implemented durable journal and explicit owner recovery | Crashes, cancellation, stale snapshots, emergency recovery and final-owner release |
| Audio | Implemented Windows WASAPI loopback, native PCM delivery and independently packaged Opus encoding; hardware unqualified | Real capture-to-encoder-to-transport qualification, clock synchronization, resampling, surround, endpoint roles and persistent recovery |
| Keyboard/mouse | Implemented authenticated session dispatch, captured-monitor coordinate validation, expiring input leases, Windows SendInput and Linux uinput backends; hardware unqualified | Native multi-monitor/mixed-DPI qualification, layout-change continuity, layouts/text, desktop ownership and crash-surviving release cleanup |
| Controller input | Pending | Supported profiles, VHF/other backends, rumble, force feedback and adaptive triggers |
| Application lifecycle | Implemented process containment, catalog/session launch, ordered preparation, reverse undo and retained cleanup failures | Windows/Linux runtime qualification, Windows graceful quit, launcher handoff, resume, durable restart reconciliation and crash-surviving cleanup watchers |
| Library/catalog | Implemented scalable manual catalog and authorized name/ID export; 10k durable entries tested | Artwork, provider reconciliation, legacy aliases and per-app overrides |
| Steam/Playnite/Lutris | Implemented explicit-root Steam discovery | Session-user discovery, sync/reconciliation, artwork, launch behavior, Playnite/Lutris providers and cleanup |
| Activity detection | Pending | Foreground/fullscreen/overlay handling without alt-tab capture stalls |
| Performance integrations | Pending | RTSS, NVIDIA settings, Smooth Motion, Lossless Scaling and Linux providers |
| Management API/adapters | Implemented explicit versioned loopback routes | Remote HTTPS, complete domain routes, selected legacy compatibility and workload qualification |
| Frontend | Implemented management pages and signed refresh-time updates; browser visual check pending | Setup, settings, library, devices, pairing, browser stream, diagnostics and maintenance |
| Diagnostics/history | Implemented volatile caller-scoped records/counters | Cross-component privileged aggregation, session history, persistence, export and crash bundles |
| Migration | Pending | Previewed import of supported settings, applications, host identity and pairings |
| Drivers and installation | Pending | Separate driver install/update flow, service registration and OS permission setup |

Windows and Linux are release targets. macOS is currently a development host only.
SteamOS and broader Linux environments remain explicit follow-up work.

## Regression sources

The reference history identified recurring cases that must become product tests:
stale WGC input mapping, capture/encoder adapter mismatches, alt-tab churn,
multi-client display ownership, mixed-protocol teardown, VHF trigger releases,
hidden audio endpoint recovery, and application watchers surviving updates.
The current unit tests cover abstract ownership and state behavior; they do not
claim those hardware-specific regressions are fixed in this new project.

## Current verification evidence

For the configuration increment, the locked full workspace test suite, strict
workspace Clippy, rustfmt check, frontend tests, configuration inventory drift
check, signed management HTTP regression, platform composition regression, and
component update regression pass on the Windows development host. The
application cwd test now compares canonical paths, and the long-child
tick guard tolerates parallel test load while remaining below the child's
30-second delay. Non-Windows hostname compilation and live target behavior
remain unverified.

The completed application lifecycle increment passed its focused application,
catalog and launch suites, the locked full Rust workspace, strict workspace
Clippy and formatting. The signed launch workflow and platform-module regression
also passed with the integrated changes. The earlier capture-geometry verification
recorded 630 passing Rust tests (the workspace run followed by the final video
module rerun; 3 intentional subprocess helpers and 6 external-oracle/network
tests ignored in the default run). The GameStream suite also passed all 101 tests
with separately compiled pinned C oracles, including all six normally ignored
compatibility gates.
Full workspace all-target
checks pass for Windows MSVC and Linux GNU, including the TLS transport and ring's
target C/assembly compilation and bundled Opus ELF/COFF static archives. Strict
Clippy also passes on both targets. The [cross-compilation setup](cross-compilation.md)
records verified temporary tools, genuine Microsoft SDK headers and reproducible
commands. These checks do not link final release binaries or qualify target runtime
and hardware behavior.

- `cargo test --workspace --locked` exercises the ABI, runtime authority and
  release behavior, persistence, configuration migration, ownership, authentication,
  HTTP parsing, and discovery. The runtime workload test completes/releases 2,000
  calls without waiting for retention expiry. These are not streaming hardware tests.
- `scripts/test_component_updates.py` loads signed libraries, replaces a component
  without changing the shell binary, rejects tampering, and recovers failed trials.
- `scripts/test_platform_modules.py` loads the actual platform composition and
  checks persisted data, configuration migration/change events, catalog CRUD,
  session leases, controlled applications, diagnostics and Steam discovery.
- State collection tests write/reopen/traverse 10,000 entries on disk, inject a
  pre-manifest failure, check caller isolation and bound serialized pages.
- `scripts/test_host_service.py` exercises continuous ticking, graceful console
  shutdown, process cleanup, bounded restart, stalled pipe delivery and no replay
  of an in-flight mutation after worker failure.
- `scripts/test_management_api.py` sends actual HTTP requests through the loaded
  management/authentication libraries, including persisted token revocation,
  denied scopes/origins/framing, configuration migration and 160 sequential
  protected reads exceeding the old retained-request ceiling.
- Windows MSVC and Linux GNU cross-target `cargo check` verify compilation only.
  Windows/Linux runtime CI, interactive client tests, drivers and hardware remain
  necessary before their corresponding acceptance gates can be closed.

- `scripts/test_web_bundles.py` stages signed frontend updates while the native
  worker keeps running, checks version-pinned assets, rejects rollback/tampering,
  and verifies same-origin API access and restrictive response headers.
- Pairing tests exercise the Gen7 transcript with independently generated OpenSSL
  certificates, identity generation/import, wrong PINs, signatures, replay,
  ownership, deadlines and durable completion. An independent Python client now
  exercises HTTP/XML/TLS pairing; an actual Moonlight client remains an acceptance
  gate, and these checks do not establish streaming interoperability.
- Display transaction tests use a fake platform to exercise journal ordering,
  ownership, cancellation, rollback and recovery. Real Windows CCD code compiles;
  no native display mutation was performed. HDR, virtual displays, Linux display
  control and automatic crash recovery watchers remain pending.
- Frontend strict TypeScript checks and six model/API tests pass. A native browser
  visual check was attempted but computer-use access to Chrome was not approved;
  rendering, interactive browser workflows and accessibility qualification remain pending.

- `scripts/test_launch_workflow.py` launches a real controlled executable from the
  management catalog through the launch/session/application libraries. It checks
  idempotent retries, scoped access, caller isolation, rejection of executable
  overrides, ordered preparation, reverse undo, failed-preparation rollback,
  explicit stop with observed child exit, and natural-exit lease cleanup. Launch
  retains the session lease while application cleanup is still pending or failed.
  This coordinates application sessions; it does not start streaming or capture.
- `scripts/test_management_domains.py` exercises pairing PIN approval, public host
  identity, client unpairing and denied transport/key-management routes over HTTP.
  It verifies display scopes and default-disabled mutation without changing a display.
- Input uses caller-owned expiring leases and tracked release cleanup. Normalized
  Moonlight keys, relative motion, buttons and scroll now route from authenticated
  active sessions. Captured-monitor absolute mapping uses validated physical
  geometry and stops delivery on geometry failure; native mapping qualification,
  layout-change continuity, layout/text support and crash-surviving cleanup remain
  pending. Native input calls are excluded from
  local tests; Windows/Linux hardware and OS permission qualification are required.
- The virtual-display adapter targets the Sunshine driver's owned temporary leases.
  Persistent monitor reclaim, other driver protocols, session/display/capture
  integration and real driver lifecycle tests remain pending. Local integration
  checks only that disabled modules load and reject device creation/injection.

- Cancellation regression tests cover queued spawn settlement before reconciliation,
  retained request/lease ownership when cancellation fails, cleanup of a newly
  created monitor whose reply is lost, and preservation of existing monitors/input
  leases on failed or cancelled retry requests.

- Native service registration copies factory descriptors, authenticates consumers,
  checks separate native grants before pointer access, and releases bindings outside
  runtime locks. Regression tests cover descriptor relocation and repeated binding.
- `scripts/test_native_media.py` loads signed DXGI capture, encoder and video
  libraries, exercises 32 typed bind/release cycles, checks absent-stream ownership
  behavior and rejects native console access. It opens no hardware.
- Media tests cover frame bounds, packet backpressure, BGRA-to-NV12 conversion,
  H.264 normalization, caller ownership and cancellation cleanup. The public C
  media header passes C11 layout assertions. Windows hardware Media Foundation
  encoding and DXGI readback compile, but real capture-to-encode output and
  independent decoding remain unqualified. Linux media, GPU texture sharing,
  HDR and protocol/session integration remain pending.

- Video cleanup tests retain unsettled requests across failed cancellation/release,
  confirm parent termination before recovering lost acceptance replies, and prevent
  a deduplication retry from creating fresh work after its parent ends. Repeated
  close preserves cleanup progress. Independent video-only unload remains unsupported;
  whole-worker teardown relies on provider destruction for final resource cleanup.

- WGC uses exact monitor selection, a joined worker owning its WinRT/D3D objects,
  and bounded CPU frame delivery. No target fallback or reconnect is implicit.
  Native Windows monitor, cursor, desktop-switching and HDR qualification remains
  required; the pinned target-retention hardware gate is still open.
- WASAPI loopback captures an explicit render endpoint or resolves the multimedia
  default once at open. It does not change endpoint roles. PCM tests cover silence,
  discontinuities, timestamps, callback ownership and bounded overflow behavior.
  Routing/default-role recovery, endpoint-change recovery, Linux audio,
  capture-to-encoder coordination and audiovisual transport integration remain pending.
- Video tests cover allowlisted DXGI/WGC routing, force-IDR retention on busy input,
  graceful capture shutdown, exactly-once EOS acceptance, sink rejection without
  further provider polling, and drain timeout cleanup with caller-scoped errors.
- `scripts/test_management_domains.py` additionally verifies `media.read` discovery
  routes over real HTTP, rejects foreign scopes and body overrides, and confirms
  no native pointer or stream-open route is exposed. Signed native tests now bind
  WGC and audio as well; default-disabled platform tests open no media streams.

- `scripts/test_nvhttp.py` uses independent Python AES/RSA operations for the full
  Gen7 transcript over real HTTP. The initial request stays open until PIN approval.
  TLS 1.2/1.3 verify and pin the pairing-owned host certificate, require matching
  client proof for final pairing, reject unpaired identities, and check that paired
  trust and revocation survive process restarts. Public `/unpair` cannot revoke trust.
  Application-list checks cover 40 entries across pages, escaped names, IDs retained
  after rename and restart, deletion without ID reuse, and revoked-client denial.
- The native identity interface signs supported RSA SHA-2 TLS messages without
  exporting private keys. Tests verify all six schemes, wrong-principal rejection,
  commit acknowledgement, revocation, validity, concurrent access and callback
  lifetime. Its public C header passes layout assertions.
- Cold activation timeouts were reproduced and traced to cumulative healthy
  startup work: a 26-component, 72.8 MiB package spent 2.292 seconds verifying and
  2.900 seconds loading, reaching READY after 5.194 seconds. The old five-second
  total deadline expired; teardown took only about 5 ms. The generic host now
  accepts finite, ordered verification/component/request progress and uses a
  five-second inactivity deadline. Duplicate, excess and out-of-phase progress
  are rejected. Fresh candidate and persistent-serve checks pass; a deliberately
  stalled module still times out and falls back to the known-good set. Existing
  heartbeat, live-request and shutdown limits remain unchanged. Installed-service
  startup and sustained target workload qualification remain open.
- Catalog export binds each authorized transport principal to one configured
  library owner. It returns only names and IDs, preserves revision-bound paging,
  and performs no writes, including legacy migration. Ordinary CRUD remains scoped
  to the immediate caller.
- The adapter supports configured paired launch/cancel; resume and self-service
  unpair remain unsupported. Application IDs map canonical catalog counters to
  positive signed 32-bit client IDs; overflow fails instead of truncating or hashing.
  Broader client profiles, display optimization and OS input remain pending.
- Application-list tests reject partial snapshots, changed revisions, malformed
  cursors, duplicate IDs and capacity overflow. Deferred successful lists recheck
  paired TLS trust before queuing output. Known broker requests remain tracked
  across cancellation/result-release failures and count against admission limits;
  retries during ticks clean them up without replaying service requests. Module
  unload still provides only bounded best-effort cleanup.
- The Rust GameStream library tests incremental RTSP framing, strict CSeq/length
  handling, bounded response serialization and typed H.264 SDR/stereo ANNOUNCE
  requirements. Parsing does not authorize a session or accept its capabilities;
  control, encryption, audio and parity support must be checked explicitly.
- Video packetization supports bounded GameStream RTP/NV framing and systematic
  Cauchy Reed–Solomon parity, with minimum parity and explicit 1–4 block limits.
  The pinned upstream C encoder matches parity byte-for-byte, and its decoder
  reconstructs missing emitted packets through the tested recovery budget.
  Multi-block output requires an explicitly compatible version profile. Tests
  do not qualify the complete Moonlight C queue/depacketizer or a real client.
- The audio encoder statically includes pinned libopus behind a Rust-owned native
  interface. Tests decode mono/stereo F32/I16 output at 5/10/20 ms packet sizes,
  verify tone fidelity, ownership, cancellation, bounded queues, discontinuity,
  capture-clock jitter and one accepted EOS. Drift is reported, not corrected.
  `scripts/test_native_media.py` additionally feeds synthetic PCM through the
  actual signed library and verifies packet retry, padded drain and EOS without
  opening hardware. The encoded-audio C header passes layout assertions.
- Platform tests verify disabled Opus policy; real HTTP tests verify scoped
  `audio_encoders` discovery and rejection of body overrides. Media routes carry
  their empty-body contract in the route definition, avoiding a second provider list.

- The managed media transport exposes a separate C ABI for complete H.264 access
  units and Opus packets. It owns UDP sockets and bounded queues, enforces the
  immediate caller's stream ownership, and remains disabled with an empty caller
  allowlist in development packages. A trusted session coordinator must authorize
  destinations and supply fresh keys; this module does not authenticate peers.
- Audio RTP uses fixed-size Opus packets and four-data/two-parity recovery groups.
  Opus encoding uses the codec's constant-bitrate mode and reports its measured
  packet size. Independent upstream C parity/recovery checks include encrypted
  audio. Video encryption uses AES-GCM; legacy audio uses unauthenticated AES-CBC
  before parity generation. Explicit IV-wrap policy exposes the legacy protocol's
  limitation rather than silently claiming authenticated long-session audio.
- Queue admission precedes packet-index and nonce consumption. Local drain means
  queued datagrams were sent to the socket, not received or decoded by a client.
  Fatal socket errors and stalled queues fail visibly. Cross-restart key freshness,
  RTSP session authorization, control dispatch, capture scheduling, audiovisual
  synchronization and actual Moonlight playback remain required integration gates.

- The signed native media integration opens the packaged encoder and transport
  under the fixture's granted principal. Python receivers independently verify
  plaintext and encrypted runs: four real 160-byte Opus packets (176 bytes after
  CBC), audio parity, video framing/parity, GCM integrity, exact source ports and
  ten datagrams followed by empty drained queues. The video input is an Annex B
  framing fixture, not evidence of H.264 decoding or hardware encoder correctness.

- A separate control-transport library now owns ENet sockets and authenticated
  GameStream control envelopes. It enforces caller ownership, fresh-key history,
  destination admission, bounded queues and authentication deadlines. Invalid
  ciphertext and replays do not deliver plaintext or refresh liveness. Native
  send admission precedes nonce consumption; rejected receive callbacks retain
  exact plaintext. ENet channel/delivery metadata is not an authorization boundary.
- The independently implemented ENet codec/peer engine passes pinned C wire
  fixtures and actual C-client handshake, reliable/fragmented echo and disconnect.
  Tests cover retransmissions, shared control replay across channels, fragment
  overlaps/gaps, sequence wrap, queue pressure and no false acknowledgements on
  rejected reliable data. Adaptive congestion shaping, compression, checksums
  and peer migration remain unsupported; payload and metadata bounds are explicit.
- `scripts/test_control_transport.py` loads signed libraries and uses the original
  pinned C ENet client with independent Python AES-GCM. Exact 0-, 2,000- and
  65,511-byte plaintext round trips verify maximum control envelopes, fragmentation,
  native sink retry, direction-specific nonces and owned teardown. This does not
  dispatch real input or qualify a complete Moonlight streaming session.
- Modern encrypted RTSP has bounded incremental envelope framing and independent
  directional counters/replay handling. Control and RTSP golden vectors, tampering,
  maximum lengths, failed-authentication state and nonce exhaustion are tested.
  The managed session library now coordinates an authenticated control peer and
  native media providers. A managed encrypted RTSP server now delegates to it;
  the configured paired HTTP launch adapter now delegates to it; complete client profile negotiation remains pending.

- `amaterasu.stream-session` coordinates prepare/authenticate/start/stop in a
  separately packaged, disabled-by-default library. It allocates transport ports
  before capture, requires authenticated control before hardware, assembles bounded
  video access units and forwards PCM through real Opus encoding. Explicit packet
  boundary opt-in preserves legacy encoder/video consumers without changing ABI
  table layouts. Video open operation IDs support reconciliation and exact retries.
- Typed modern GameStream control decoding validates lengths, direction and input
  record shapes. The coordinator coalesces IDR/reference invalidation into a
  keyframe request; other input remains counted without OS effects. Invalid or
  wrong-direction control never becomes a stop command. ENet metadata grants no
  additional authority.
- Coordinator tests cover shared capture leases, resolved audio identity,
  pre-authentication retries, failed completion, parent cancellation, uncertain
  broker admission/cancellation, cleanup retries and video teardown confirmation.
  Unconfirmed cleanup retains ownership visibly. Unload releases native bindings
  and attempts broker settlement; asynchronous orderly cleanup requires a stop
  before replacement or provider unload/process shutdown as a backstop.
- `scripts/test_stream_session.py` loads the signed synthetic-source composition
  and uses real Opus, UDP media, control transport and the pinned ENet C client.
  Independent Python verifies eight fragmented video access units, GCM and video
  parity, nine CBC-encrypted Opus packets including drain and audio parity, source
  ports, authenticated IDR dispatch, idempotent stop and balanced source cleanup.
  A separately opened unsupported audio format fails with both sources closed and
  no media sent. Video remains a framing fixture, not decoder or hardware proof.
  The production composition retains real providers and disabled session policy.
- The coordinator currently accepts H.264 SDR with 48 kHz stereo PCM and 5/10 ms
  Opus. Its shared output lease identifies provider plus output, not a canonical
  physical display. Complete client profile negotiation, OS input
  dispatch, display/application restoration, capture pacing, audiovisual clock
  correction, Linux providers and real client/hardware qualification remain open.

This increment's local evidence is retained in
`/private/tmp/amaterasu-stream-workspace-tests-final.log`,
`/private/tmp/amaterasu-stream-clippy-final.log`,
`/private/tmp/amaterasu-stream-oracles.log`,
`/private/tmp/amaterasu-stream-integration-final.log`, and
`/private/tmp/amaterasu-stream-platform-retry.log`.
Native media/control regression logs use
`/private/tmp/amaterasu-stream-test_{native_media,control_transport}.log`.
The failed initial platform activation remains recorded in
`/private/tmp/amaterasu-stream-test_platform_modules.log`.


- The separately managed RTSP library accepts caller-owned authorized tickets and
  keeps encrypted request counters/replay windows across TCP connections. Bounded
  transport tests cover partial reads, wrong keys, plaintext rejection, same-IP
  ticket selection, replay, disconnects, revocation and read/write/router limits.
  Strict negotiation checks OPTIONS/DESCRIBE/SETUP/ANNOUNCE/PLAY/TEARDOWN ordering,
  targets, CSeq, session tokens, launch epochs and pending completion tickets.
- `start_when_authenticated` acknowledges an armed lifecycle without opening
  capture. This follows the client's PLAY-before-control order. The original
  prepare deadline is not extended by retries; after authentication, existing
  startup and rollback apply. Tests cover cancellation, lost completion, owner
  isolation, timeout, stop before authentication and autonomous startup failure.
- The H.264 configuration accepts an optional one-reference requirement. The
  Windows backend sets and verifies the codec property before and after media
  type commitment and after stream changes; video requires encoder confirmation.
  Default zero retains provider choice and is omitted on serialized legacy calls.
  Video likewise omits a disabled packet-boundary option when opening an encoder.
  Readback and compilation do not replace hardware bitstream qualification.
- The signed RTSP integration uses an independent Python AES-GCM client and the
  pinned ENet C client. It verifies modern negotiation, replay rejection across
  connections, PLAY before control, no capture before control authentication,
  synthetic video and real Opus/encrypted media, confirmed TEARDOWN, and an
  unsupported ANNOUNCE response with no new capture. RTSP protocol state and
  actual media-session state remain distinct in status.
- The ticket API supports fixed client media ports or token-based UDP destination
  discovery. SETUP's client-port placeholder is never destination authorization.
  The listener is encrypted-only and explicitly bound; no wildcard address is returned
  as a usable URL. Display restoration, broader input profiles and hardware/client
  playback remain open.


Latest combined verification logs are
`/private/tmp/amaterasu-rtsp-workspace-tests-final.log`,
`/private/tmp/amaterasu-rtsp-clippy-final.log`,
`/private/tmp/amaterasu-rtsp-oracles.log`, and
`/private/tmp/amaterasu-rtsp-final-test_rtsp.log`.
The initial reproduced platform timeout remains in
`/private/tmp/amaterasu-rtsp-platform.log`; additional fixed-host regression logs use
`/private/tmp/amaterasu-rtsp-final-test_{platform_modules,host_service,component_updates}.log`.


- UDP discovery is implemented across the managed RTSP, stream-session and media
  transport libraries without shell or ABI layout changes. RTSP generates distinct
  96-bit audio/video bearer tokens and emits each only in its encrypted SETUP reply.
  The trusted peer IP and exact 20-byte Moonlight ping select each UDP source port
  once. Wrong IP, malformed length, wrong lane and zero sequence are rejected;
  subsequent pings cannot rebind a selected port. Tokens are absent from public
  status, and media transport retains bounded token-reuse history across closes.
  Bearer evidence does not provide cryptographic UDP sender authentication.
- Prepare binds sockets before SETUP. PLAY arms immediately; capture and resource
  leases wait for authenticated control and both healthy media destinations.
  Pending discovery consumes no media indices/nonces. Discovery has a fixed
  five-second deadline, checked before queued pings; session polls and retries do
  not extend the original prepare deadline. Failure and stop preserve existing
  confirmed cleanup. Fixed-port JSON requests remain compatible, including older
  provider responses that omit the additive readiness fields.
- The signed RTSP integration now covers both fixed and discovered destinations,
  audio ping after SETUP, video ping after PLAY/control, malformed/wrong-lane
  rejection, rejected audio-port rebinding and no capture with one pending lane.
  It independently verifies the resulting encrypted video/FEC and real Opus/audio
  traffic from the actual server ports, then confirms TEARDOWN cleanup. Synthetic
  H.264 remains a framing fixture. Complete public launch profiles, NAT network
  qualification, reconnect/resume, input dispatch and actual client/hardware
  playback remain open.

Current discovery verification logs are
`/private/tmp/amaterasu-discovery-workspace-tests.log`,
`/private/tmp/amaterasu-discovery-clippy.log`,
`/private/tmp/amaterasu-discovery-oracles.log`, and
`/private/tmp/amaterasu-discovery-test_rtsp.log`.
Windows/Linux workspace check and strict Clippy logs use
`/private/tmp/amaterasu-cross/discovery-{target}{-clippy}.log`.
The signed RTSP, native-media, stream-session and platform integrations all passed;
regression logs use
`/private/tmp/amaterasu-discovery-test_{rtsp,native_media,stream_session,platform_modules}.log`.


- Catalog `resolve_exported_launch` requires both an authorized launch provider
  and a startup export mapping for the immutable initiating principal. Public
  requests cannot supply the catalog owner, executable or environment. Managed
  `launch_export` uses the canonical owner/app lease shared with ordinary launch;
  retries retain the original initiator separately from the resolved owner.
  Caller-owned launch lookup supports reconciliation of uncertain allocation.
- Optional NVHTTP streaming configuration selects capture and media policy. The
  client chooses even dimensions and frame rate within configured maxima. Public
  launch requires paired TLS, confirms an active managed application, then opens
  its owned encrypted RTSP discovery ticket. Both operations must succeed before
  launch XML is returned. The exact certificate owns cancellation; remote key IDs
  are IV salts, while AES keys determine retry/reuse identity.
- Launch adoption waits for the HTTP response and TLS close notification to be
  written to the socket, which does not prove client receipt. Missing delivery,
  revocation, application exit and RTSP termination trigger owned cleanup. One
  side's cleanup failure does not prevent stopping the other. Multiple uncertain
  broker calls remain retained; explicit retry settles them before reconciliation.
  Cleanup deadlines do not reset on repeated cancellation/revocation, and live
  monitoring has its own deadlines independent of initial launch.
- The initial public profile is deliberately explicit about remaining work:
  display optimization (`sops=1`), HDR, surround, local-audio and controller
  forwarding requests are rejected before application effects. Other wire profile
  parameters still come from installed policy and must match ANNOUNCE. Resume,
  complete client negotiation, display optimization and restoration, broader
  per-application policy, OS input, durable restart/reload reconciliation and real
  client playback remain required. Startup streaming advertisement describes
  configuration, not a hardware capability probe. Development listeners and
  streaming remain disabled by default.

Public-launch workspace and Clippy evidence is in
`/private/tmp/amaterasu-public-launch-workspace-tests-final.log` and
`/private/tmp/amaterasu-public-launch-clippy.log`.
The initial UDP test timing failure is retained in
`/private/tmp/amaterasu-public-launch-workspace-tests.log`; the test now waits for
observable endpoint discovery within a bounded deadline instead of assuming a
single immediate receive tick.

`scripts/test_public_launch.py` passes against the signed synthetic composition.
It pairs over real HTTPS, launches a controlled catalog application, verifies
stable owner retries and rejects another paired certificate's cancellation.
Natural process exit reaches the FREE cleanup state. Three fresh-key sessions
reuse key ID 17 and independently verify encrypted RTSP, UDP discovery and media;
TEARDOWN, owner cancel and certificate revocation each close capture and reap the
application. Untrusted or owner-override requests produce no application/capture
effects. Evidence is in `/private/tmp/amaterasu-public-launch-integration.log`.
The two initial fixture-assumption failures are preserved as `-initial.log` and
`-foreign-cancel.log`: cleanup is synchronized by FREE rather than `currentgame=0`,
and foreign cancellation is an explicit 403.

Candidate verification now reports finite ordered manifest, artifact and component
milestones. The five-second inactivity watchdog still applies to each silent
phase; duplicate, skipped or out-of-phase milestones fail activation. Package
callback failures stop verification before native loading. Package tests (21),
host tests (5), targeted Clippy and the original signed launch workflow passed;
logs are `/private/tmp/amaterasu-artifact-progress-{package-tests,host-tests,clippy}.log`
and `/private/tmp/amaterasu-launch-workflow-artifact-progress.log`.
Signed host-service and component-update regressions also passed in
`/private/tmp/amaterasu-artifact-progress-final-test_{host_service,component_updates}.log`.
The host watchdog regression now checks that the stopped worker is reaped within
seven seconds separately from replacement readiness. The old eight-second
combined assertion included both the five-second watchdog and full native
startup; its failure is retained in
`/private/tmp/amaterasu-artifact-progress-test_host_service.log`.

Authenticated keyboard/mouse dispatch now runs inside the managed stream-session
library when trusted startup policy enables input. Pre-active input is discarded;
activation acquires an operation-bound input lease before reporting active. Keys,
relative motion, buttons and scroll use bounded translation and queues. Generic
and left-side modifier aliases share ownership; native and portable key encodings
cannot be mixed within one lease. Missing authoritative capture geometry keeps
absolute input unsupported. Text, controller and haptic events remain unsupported.

The session renews leases using a conservative submission-time deadline. Overflow,
ambiguous or partial batches, expiry and renewal failure stop input and initiate
cleanup. Ambiguous batches are never resubmitted as new effects. Owner/operation
lookup can recover an expired or unhealthy lease for release; a failed second
session cannot adopt the first session's lease. Input release precedes media drain,
and unresolved cleanup remains retained. The input provider retains terminal
acquire results for five minutes; session recovery does not treat that bounded
history as permission to replay a completed acquisition.

An additional signed input run now verifies captured-monitor absolute mapping
using fixed synthetic geometry with negative desktop origins. The same test
checks missing-geometry rejection, input lease renewal, and held-input release
after the synthetic video provider reports geometry failure. The session requires
both validated geometry and the input provider's explicit absolute-pointer
capability. Evidence is `/private/tmp/amaterasu-geometry-signed-input.log`.
This exercises loaded session/input interfaces, not Windows capture enumeration
or native pointer delivery. Production capture/video geometry integration is now
implemented, with mixed-DPI/hotplug qualification still open. The additive capture v1.1 C header
also passes the C11 layout assertions in `sdk/tests/media_layout.c`; consumers
must check the advertised version and size before reading its optional tail.
Those assertions pass with host Clang, Windows MSVC-target Clang using the
Microsoft headers, and Linux-target Zig compilation to an x86-64 ELF object.
The signed native-media integration also passes separate legacy-prefix and
v1.1 geometry bindings to both capture providers, requiring absent-stream
isolation on each of 32 iterations. Its software Opus and loopback UDP regression
checks remain green in `/private/tmp/amaterasu-geometry-native-media.log`.

Production video checks the capture-owned geometry before encoder creation,
again before activation, and before each scheduled capture frame. Missing
geometry remains compatible with older providers; malformed metadata or loss of
an established geometry closes packet delivery and initiates owned cleanup.
The Windows providers reconcile GDI/DXGI/CCD rectangles and physical target
identity inside a scoped PMv2 coordinate context, with CCD snapshots bracketing
enumeration. This does not provide atomicity against subsequent hotplug.
The final video suite passes 45 tests, including activation deferral and cleanup
on changed/lost geometry. Host workspace tests and strict Clippy pass, as do
formatting and final Windows/Linux workspace all-target checks and strict Clippy.
Evidence is `/private/tmp/amaterasu-geometry-workspace-tests.log`,
`/private/tmp/amaterasu-geometry-video-final.log`,
`/private/tmp/amaterasu-geometry-clippy.log`,
`/private/tmp/amaterasu-geometry-fmt-final.log`, and
`/private/tmp/amaterasu-cross/capture-geometry-{target}{-clippy}.log`.
The final signed regression batch passes input, native media, RTSP,
stream-session, paired public launch, and platform services against this
integrated geometry build. Logs are
`/private/tmp/amaterasu-geometry-final-test_{stream_input,native_media,rtsp,stream_session,public_launch,platform_modules}.log`.
These use synthetic capture or unopened native devices; they do not qualify
real capture, native input delivery, drivers, or full client playback.

`scripts/test_stream_input.py` passes against signed modules and the primary C ENet
client with an in-memory input provider. It verifies pre-active rejection, decoded
keyboard/mouse effects, replay rejection, unsupported absolute input, renewal and
held-input release. Production profiles retain the native input library; only
StreamProbe substitutes the disabled-by-default synthetic provider. Evidence is
`/private/tmp/amaterasu-stream-input-integration-final.log`. The first fixture
response omitted the production `cleanup_required:false` field; that failure is
preserved in `/private/tmp/amaterasu-stream-input-integration.log`.

Signed RTSP, stream-session, public-launch and platform regressions pass in
`/private/tmp/amaterasu-input-test_{rtsp,stream_session,public_launch,platform_modules}.log`.
The RTSP regression also caught an in-progress activation guard that prevented
media drain during stopping. The guard now permits draining; a focused fake-plane
test requires an actual media tick after stop. The original failure is retained
in `/private/tmp/amaterasu-artifact-progress-final-test_rtsp.log`.
These checks do not inject native input or qualify Windows/Linux desktop behavior,
captured-monitor mapping, layout/text handling or crash-surviving key release.

Unknown-admission retries are now bounded by the runtime's five-minute result
retention. Once that window has elapsed, cleanup cannot recreate the old request;
it uses operation lookup to find and release any retained allocation. Shutdown
also never sends an unsent or unknown-admission mutation. A delayed cleanup test
proves that only one acquisition is submitted after advancing past retention.
Final input verification logs are
`/private/tmp/amaterasu-input-retention-{workspace-tests,clippy,fmt,signed,rtsp}.log`;
all six external compatibility gates pass in
`/private/tmp/amaterasu-input-compatibility-oracles.log`. Full all-target checks
and strict Clippy also pass on Windows MSVC and Linux GNU in
`/private/tmp/amaterasu-cross/input-retention-{target}{-clippy}.log`.
