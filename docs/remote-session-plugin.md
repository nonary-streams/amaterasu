# Remote session plugin and SDK contracts

Remote Monitor and Remote Input are supplied by the independently built
`amaterasu.remote-session` provider. NVHTTP consumes the public versioned
`nonary.session-presentation/1` contract through the Module SDK. Product
policy, control identities, per-client catalog projection and retained monitor
ownership live in the provider. The host shell does not implement these modes.

The behavioral reference is local Vibeshine revision
`43cf27252a205221a295126a5fcd9ad87ba9c914`, including `remote_session`, its display
topology coordinator and NVHTTP integration. This implementation uses
Amaterasu's contracts and provider composition; it does not copy or translate
that implementation.

## Public contracts

The SDK separates authenticated client facts, running-application observations,
catalog presentation and launch decisions. Per-client projection transactions
stream bounded catalog pages through the broker. Consumers release abandoned
projections and recheck the context before returning the complete app list. A
projected entry is never authorization to launch or terminate an application.

Catalog-list permission is independent of stream-view permission. A list-only
client can read its projected catalog; a view-only client cannot obtain catalog
rows through a projection transaction. Resume, auxiliary launch and termination
each retain their own permission checks, including a fresh authenticated check
before effects or a reply is delivered.

The provider can return ordinary launch, attach, replacement, stop, auxiliary
stream and completed-control decisions. Auxiliary stream plans carry logical
capture selectors and resource requirements. They contain no OS handle, native
allocation pointer or provider identity. Trusted startup composition chooses the
capture, input, display and other native implementations.

The black capture component implements the same typed CPU Capture SDK available
to external developers. Remote Input can therefore preserve protocol video
traffic without acquiring a host display. Remote Monitor supplies a logical
exact output selector after the display provider verifies readiness; a missing
target fails instead of selecting another client's display.

Display cleanup reads caller-scoped journal ownership before requesting native
desktop topology. The SDK requires an explicit `reference_owned` result: a
reserved but unapplied reference remains owned even without an operation ID.
Only a proven absent reference, with no capture holds and verified native
reference retirement, permits cleanup without accessing a desktop. Unknown or
pending placement work continues through reconciliation.

Before borrowing a retained source, the selected NVHTTP transport obtains
`VerifySource` and durably binds the exact reservation to its RTSP operation and
runtime epoch through `BindTransport`. The lifecycle coordinator accepts only
the explicitly approved `borrowed_capture_callers` in external binding mode and
freezes that attestation with the application session and its fresh stream
reservation. Required lifecycle hooks still execute. The public
`borrowed_capture_context` helper exposes source mode separately from requested
stream mode through the provider's frozen application context. The attestation
does not grant native mutation authority or independently prove readiness.

## Behavioral boundaries

The provider owns authenticated client roles, stable synthetic controls, catalog
collision handling, cached running-game tiles, bounded confirmations and
retained monitor profiles. NVHTTP supplies its current application state and
transport-authenticated client identity. Query parameters do not confer client
ownership. Protocol app-list and server-info projection are evaluated for that
client; another client's retained monitor is not published as the current game.

Control operations are distinct from streaming launches. A successful
Disconnect or Terminate control returns its protocol completion only after its
required effects complete. Normal application termination preserves unrelated
Remote Monitor and Remote Input ownership. Monitor disconnect uses a matching
owner and generation and retains uncertain cleanup for reconciliation.

Resource lifetime is separate from transport lifetime. A Remote Monitor may
retain its display after the stream ends, according to the disconnect settings.
Its native ownership reference is separate from a stream's capture/input
resources. Reconnects and late disconnect callbacks cannot release a newer
generation or a peer's display. Configured monitor profiles freeze the accepted
selection so subsequent catalog edits cannot silently retarget retained
ownership.

Saved layouts use version 1 placements keyed by authenticated client identity.
Each placement names a physical or client anchor, edge, alignment, pixel gap and
optional primary/fallback behavior. The plugin validates the complete graph and
persists it through the revisioned configuration provider. Its public
`layout_get` and `layout_update` methods report both requested placement and
observed application state. The signed UI uses these methods rather than
writing display settings directly.

Monitor profile CRUD uses the separate `profiles_get`, `profile_upsert` and
`profile_delete` methods. The separate `profile_targets` method returns
sanitized, provider-verified target inventory so a profile can select an existing
output without entering device paths. The catalog approves
`amaterasu.remote-session` only
for inert marked monitor records in the same owner namespace exported to
NVHTTP. Ordinary executable applications cannot be read or changed through this
profile facade. Profile writes use the catalog revision; layout and preference
writes use the configuration revision.

## Package composition

Platform and Linux session package graphs include `amaterasu.remote-session`
and `amaterasu.capture-black` as independent native components. NVHTTP explicitly
selects the presentation service and auxiliary capture routes in bootstrap
settings. Method approvals derive directly from the SDK schema and name NVHTTP
as the authorized protocol caller. The separate public settings alias
`nonary.remote-session/1` is approved only for management.

The capture call grant remains present through NVHTTP, RTSP, stream-session and
video, because parented requests intersect ancestor grants. Only video receives
native capture-use authority. The remote-session provider depends on the
selected configuration, state, display and virtual-display components. Package
composition grants no new host input, display mutation or network access;
hardware-dependent defaults remain disabled until the existing operator policy
enables them.

Remote-session compositions explicitly allow up to 32 live RTSP transports and
32 stream sessions. This bounds the complete transport pool; the plugin's
`max_clients` preference separately bounds retained remote owners. Each live
transport reserves three UDP ports at a checked offset of 0, 3, …, 93 from the
configured base stream ports. Cleanup uncertainty retains its slot until
retirement is verified. The package does not modify firewall rules or open
listeners while it is being prepared.

Monitor recovery may inspect RTSP retirement through the separately approved
`retirement_status_owners` map, pinned to the selected NVHTTP transport owner.
The optional legacy observer list is valid only with that exact owner mapping.
Disabled seeds without an approved transport owner receive neither an owner
nor an observer. This read-only grant does not place the plugin in RTSP's
ordinary open/close caller list. Display mutation
verifies the retained owner through Virtual Display's separately approved
`target_verifiers` list. Both grants name exact startup-selected principals.

Application retirement inspection uses a separate read-only
`application_retirement_observers` map bound to the selected NVHTTP launch owner.
The remote provider is not added to the launcher's ordinary launch/stop callers
or its legacy mutating retirement verifier list. Pairing authorization is read
through its separately approved `authority_observer_principals`; the plugin is
not made a pairing administrator or transport authority.

The signed UI declares scoped preference, layout and profile aliases. It uses
optimistic revision checks and preserves a draft when an update fails. Neither
the public API nor the UI can modify trusted caller lists, selected providers,
publisher approvals or native package selection.

Video's signed `source_preflight_routes` map binds each logical route to both an
approved provider and a source class. The same provider may serve managed and
existing outputs under different permissions. This class is bound to the
logical route, not inferred from a provider identity or output prefix. The
remote provider is included only in Video's `preflight_only_principals`, which
rejects its ordinary video open/close operations. Ordinary eight-bit SDR capture
retains its legacy request shape; features that require higher depth probe the
declared optional format contract before opening a capture.

Linux physical-output capture has a separate explicit
`--enable-desktop-output-capture` initialization option, requiring
`--enable-capture`. Its signed `allow_desktop_outputs` permission is independent
of the existing managed-only `allow_named_outputs` permission. Packaging and
service preflight require the linked physical-capture feature marker from the
exact PipeWire artifact. The scoped KDE executable declaration is still required.

CPU HDR processing is an optional external-provider selection. Video's signed
`resource_processor_service` binds the existing
`amaterasu.frame-resource-processor.native/1` family through the public
`processing::resource` SDK. It is mutually exclusive with
`cpu_processor_service` and `gpu_processor_service`; composition supplies the
exact native-use grant and dependency, and the lifecycle processing selection
must name that same service. Version 1.0 accepts empty per-stream settings and
does not support live updates.

Source and output color/format offers negotiate independently. The provider
must transform the pixels and retire its work; changing metadata is not a
conversion. The SDK and unchanged Video consumer validate the selected offers,
source provenance and output ownership. Replacement fixtures verify neutral
black output codes and lifecycle behavior, rather than a general tone-mapping
algorithm. No stock resource processor or CPU tone mapper is selected. See
[CPU resource processing](resource-frame-processing.md) for the contract and
composition example.

## Verification and remaining qualification

The implementation has distinct verification layers:

| Layer | Evidence required |
| --- | --- |
| SDK | Exact method schema registration and an unchanged consumer using a replacement implementation. |
| Provider policy | Stateful tests for client-specific projection, controls, confirmation, collisions, reconnect and cleanup decisions. |
| Protocol and stream consumers | Tests for projected wire responses, bounded/cancelled requests, auxiliary concurrency and isolated resource cleanup. |
| Package | Acyclic dependency graphs, complete nested call grants, explicit capture registration and exact approved method identities. |
| Native platform | Real monitor readiness, exact capture, HDR/mode observation, peer-safe topology and verified removal. |
| Client playback | Actual Moonlight launch, reconnect, input/feedback and displayed video/audio. |

Source tests and package construction do not establish the last two rows. This
change does not install a package, restart a service, publish a repository or
claim Windows/Linux client playback qualification. Existing broad parity
inventories retain pending status until the complete target-platform acceptance
criteria pass.

The stock Windows MFT encoder is limited to NV12/H.264 and does not encode
ten-bit SDR or HDR. New high-depth capture and capability-based Video routing
support selection of compatible SDK encoders; a capable replacement and native
qualification are required for those modes. The Linux KWin PipeWire CPU capture
path remains BGRA8 and cannot supply HDR physical-output pixels. These are
confirmed source capability boundaries, not merely missing hardware tests.

Windows-style CPU-v2 P010 HDR capture with an SDR client also requires an
explicitly selected compatible CPU resource processor. The stock GPU effects
consume DMA-BUF leases and cannot process that CPU source. The new SDK/Video
route enables a replacement to provide HDR-to-SDR conversion; default
composition still rejects it. This does not add HDR encoding to the stock MFT
encoder or recover HDR pixels from the BGRA8 PipeWire source.

Local SDK exports and VKMS candidate pins used for validation are unpublished
implementation artifacts. A local build or passing qualification does not
publish those dependencies to a shared repository.

The executor's current retirement proof does not attest native resources from a
previous runtime epoch. Such a source stays held until its owning executor can
produce authenticated cleanup evidence. A service restart is not retirement
proof; crash recovery of that case remains a specific qualification limit.

The isolated signed native acceptance case passes loading and shutdown, paired
client-specific catalog presentation, Remote Input transport, independent
FFmpeg decoding of 23 black frames, held-input neutralization, input lease
release and transport cleanup. It also passes unavailable-monitor cancellation
and conclusive missing-provider rejection. Native input is supplied by an
in-memory probe; these results do not establish OS input delivery, monitor
creation or Moonlight presentation.

Windows source compilation checks cover DXGI, WGC, Video, Audio, Encode,
Display, Virtual Display and Remote Session using an isolated matching compiler
and Windows standard library. These are compile checks, without Windows linking,
driver execution or hardware qualification. The public SDK also exports the
small Windows capture geometry helper used by the built-in capture providers.

For isolated signed native acceptance, build the complete Linux session profile,
the in-memory input probe, host and release utility, plus software x264 encoding
and the interactive ENet oracle. Run the harness against those exact artifacts:

```sh
python3 scripts/test_remote_session_plugin.py \
  --sdk-root /path/to/amaterasu \
  --artifacts /absolute/build-volume/target/debug \
  --oracle /path/to/interactive-enet-oracle \
  --work-root /absolute/build-volume/remote-session-qualification \
  --keep
```

The harness signs only its newly generated private development fixture, replaces
native input with the input-probe artifact before starting a host and disables
all physical/virtual display, desktop-capture and audio effects. It checks actual
native module loading, exact API identity, client-specific projection, paired
loopback Remote Input, decoded black pixels and explicit input/transport cleanup.
A separate missing-provider fixture checks that the selected service cannot fall
back to the earlier successful package. A passing run remains distinct from real
monitor, OS-input and Moonlight playback qualification.
