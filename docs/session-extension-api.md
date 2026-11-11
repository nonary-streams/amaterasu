# Session extension API

Amaterasu provides session lifecycle, extension settings, and action services as
independently built managed libraries. Display behavior belongs to independently
built display-settings and virtual-screen providers. The shell loads approved
components and routes their broker calls; it does not contain display policy.

## Services and installation

| Service | Responsibility |
| --- | --- |
| `amaterasu.session-lifecycle/1` | Durable session transitions, provider admissions, consumer bindings, retirement, and recovery. |
| `amaterasu.extension-registry/1` | Approved descriptors, typed settings, revisions, and authorized scope resolution. |
| `amaterasu.extension-actions/1` | Action planning, durable invocation receipts, cancellation, and lifecycle permits. |
| `nonary.virtual-screen/2` | Session-owned virtual-screen resources, native leases, and owner callbacks. |
| `nonary.display-settings/2` | Stream display configuration and restoration callbacks. |

Use [the opt-in local profile](session-extensions-local-profile.md) to compose a
package. Components, registered method schemas, callers, and grants must agree.
Native modules are trusted code; broker grants are not a native-code sandbox.
Neither the test profiles nor a successful package verification deploy a service.
The generic `amaterasu.host-state-context/1` call grant lets an approved module
request the trusted host's runtime state root. It does not expose package metadata
or allow a module setting to substitute a different root.

## Lifetimes and callbacks

`session.prepare` runs before application launch. `stream.prepare` receives the
negotiated mode before capture opens. The coordinator publishes a candidate only
after provider preparation succeeds, and starts delivery only after the required
capture and input consumers acknowledge the exact binding.

Disconnect means **pause while retaining the application**. Pause first fences new
input/actions, then waits for native consumer retirement, then calls
`stream.release`. Resume admits a new stream identity. Ending additionally waits
for the launch service to confirm application retirement before `session.release`.
Cleanup runs in reverse provider order and attempts independent participants even
after a cleanup failure. Failed preparation cleans up successfully prepared
participants; uncertain effects remain fenced for recovery.

The SDK exports `ProviderRequest`, `ProviderResponse`, `ResourcePlan`,
`CaptureBinding`, typed option schemas, descriptors, and keybindings through
`amaterasu_module_sdk::extension_api`. A provider normally registers
`resource_plan`, `lifecycle_callback`, `lifecycle_status`, and `binding_validate`.

Provider requests contain an opaque target, transition and operation IDs,
monotonically increasing **non-contiguous** provider sequence, phase, settings
snapshot/revision, requested mode, consumer requirements, and remaining budget.
Effect callbacks additionally carry their admitted plan and admission ID. Status
queries refer to the original operation and sequence; querying status must not
replay an effect.

An approved participant can request `read_provider_context` through the SDK
while the transition containing its exact planning or lifecycle callback remains
pending. The coordinator
verifies the broker caller and the complete durable provider request before
returning the application reference, requested video mode, and any client facts
supplied by the trusted owner for that particular transition. A participant
also needs an explicit broker call grant and lifecycle operation grant. Unknown
facts remain absent. NVHTTP supplies the paired client reference, explicit
display-mode override and opaque client properties for initial preparation and
each stream or resume. Application properties and executable metadata come
from the admitted preflight. The coordinator reads only the current
transition's values. Client references and properties are descriptive context,
never authority; registry reads still require independently granted access to
the selected scope.

The selected coordinator freezes effective provider settings separately for
initial preparation and each new or resumed stream. It derives the application,
paired client, and session selectors from the admitted transition and resolves
them through the registry with host < application < paired client < session
precedence. Providers receive this frozen snapshot; they do not parse another
plugin's settings or ask transport modules to apply overrides.

Registry installation grants accept exact scopes and explicit class-wide
permissions: `all_applications`, `all_paired_clients`, and `all_sessions`. These
are trusted configuration permissions, not valid request selectors or stored
layer identities. Both the broker caller and initiator must hold the applicable
permission; class-wide reads never imply writes. The default composition gives
the coordinator read access and the management/console editors read and write
access to the selected extensions. Ordinary dynamic reads still require the
existing live-target authorization. Generic plugin editors in the application
and paired-device views save only their selected scope and preserve parent
settings when an override is removed.

Descriptors may provide a `display_name`, and enum options may provide
`choice_labels` keyed by their wire values. These are bounded plain-text display
strings; the generic editor still saves the original option IDs and enum values.
Providers own the wording and option order. Host-level edits are saved directly;
editing an inherited application or device value selects an explicit override.

The SDK defines a generic `LaunchEnvironmentContribution` and bounded merge
helper. Contributions can set/remove values or extend delimited values while
preserving inherited entries. Conflicting edits are rejected. A participant
declares the `amaterasu.launch-environment` capability and a digest-pinned
`launch_environment` requirement in its plan's constraints. The coordinator's
ordinary capability admission applies; a descriptor alone grants no rights.

An approved launch consumer uses `request_prepared_plans` to read the agreed
constraints after session preparation succeeds. The coordinator requires an
explicit `read_prepared_plans` operation grant, the exact session target, and
the frozen application operation and owner. Reads are available before the
application is bound, and stop when the session ends or requires recovery.
The consumer does not receive new resource-mutation authority from these data.
The launch module selects its coordinator through the trusted
`prepared_context_service` setting and freezes the merged environment before
creating its application session. It does not inspect provider-specific policy.

Selected consumers can use the SDK's `request_frozen_provider_settings` to
read the settings snapshot for one provider at an exact ready session or
ready/active stream target. Installed lifecycle policy must grant both the
operation and the exact caller/provider pair via `provider_settings_readers`;
the provider must also be present in that target's frozen participant graph.
The response includes the target, provider and revision so consumers can
validate the result and pass opaque settings to the owning provider. A session
and a stream have independently frozen snapshots; consumers must not assume
they match. These reads grant no resource mutation authority.

An authorized stream consumer can also poll `request_live_provider_settings`
for an exact ready or active stream and selected provider. The response may be
pending while the coordinator resolves the stream's admitted scopes through the
settings registry. A successful response is a bounded complete settings object:
only options declared `live_action` both at admission and at the current read
can differ from the frozen stream snapshot. Its opaque revision identifies the
registry observation, not successful application by a consumer or provider.
The consumer must check the returned target and provider and handle a rejected
read when the stream ends or its generation changes. The lifecycle operation
and caller/provider pair both require explicit grants. Saving settings alone
does not establish that any running stream applied them.

Built-in launch, RTSP and stream-session messages do not accept legacy
frame-limiter policy payloads or invoke the limiter's apply/status/restore RPCs.
The standard package no longer grants those calls to streaming consumers. A
plugin's compatibility service is available only to callers explicitly granted
that service by a separate composition.

Plans identify owned resources, mutation domains, expected revisions, affected
consumers, cleanup obligations, and an optional capture constraint. The provider
rechecks native state immediately before effects. The coordinator holds admission
rights until the callback settles, including across asynchronous status polling.
Native capture evidence is provider-specific; the coordinator preserves it and
checks the common negotiated mode and exact binding identity.

### Dependent-provider composition

The split uses separately approved `nonary.virtual-screen/2` and
`nonary.display-settings/2` services, built independently in the
[virtual-screen](https://github.com/nonary-streams/amaterasu-virtual-screen) and
[display-settings](https://github.com/nonary-streams/amaterasu-display-settings)
repositories. Virtual-screen owns the session-retained producer and its native
leases; display-settings consumes its selected output and applies configuration
for the stream and restores it on cleanup. A resumed stream receives a fresh
configuration while the session's producer may be retained. The existing v1
provider contracts remain unchanged; the legacy single-provider profile does
not implement this composition.

The session-owned output has a resource generation distinct from the session
and stream generations. Dependent requests bind to the selected producer output
and its resource generation; a stale generation cannot authorize effects on a
replacement. Desired activation and observed native state are recorded separately;
an intended on, off, or retained state is not proof of native readback.
Disconnect retires the stream and its configuration while retaining
the application and eligible session resource. Session end additionally requires
application retirement and release of the root resource after its dependents.
The test coverage and its native-effect limits are described under Validation.

The coordinator must validate the installed dependency graph, schemas, and
method approvals before effects. It must persist the producer's selected output
and cleanup obligations before admitting dependent operations, and bind those
operations to the exact target, resource incarnation, and generation. A resource
identifier or handoff payload does not grant authority to use a native lease:
the owner performs authorized effects under coordinator-issued rights. Capture
is published only after the dependent result and producer are validated and the
consumer acknowledges the exact binding.

The selected capability is `amaterasu.display-target`. Its frozen schema
references are `amaterasu.display-target/1` (SHA-256
`ededc620113268bcae3990d492deeef039c5e9c5ff28067a676dd992fe6324ce`)
and `amaterasu.display-control/1` (SHA-256
`c2c3ce866277cb29bbd9fbd947899917538d46b1020df3197fbd264159607d87`).
The owner service `nonary.virtual-screen/2` uses
`resource_control`, `resource_finalize`, and `resource_status` methods.

Release and rollback proceed in reverse dependency order. Independent cleanup
continues after a failure, but an unresolved dependent effect or cleanup must
fence release of its prerequisite. Recovery queries recorded operations rather
than replaying unknown effects. The selected graph, settings snapshots, and
inputs must remain stable for recovery; there is no silent provider substitution
after effects. Live platform and hardware qualification remain separate from
the simulated integration tests.

The first consumer adapter accepts Linux KDE/Wayland SDR bindings through
`amaterasu.capture-pipewire/1` and `amaterasu.kwin-wl-output/1`, with PipeWire
explicitly selected. Windows, HDR, and other capture adapters still require
implementation and qualification before they can use this lifecycle path.
Legacy display mutations must be disabled when the extension owns display
configuration; conflicting global, application, or client policies are rejected.

`verify_provider_epoch` authenticates a selected provider's exact durable request.
Its fingerprint is lowercase SHA-256 of the SDK's typed JSON serialization. The
reply also contains the canonical parent `session_target` and whether the known
session consumers are retired. Payload identity fields alone never authorize an
effect.

Retained plans carry their original target and lifecycle phase. Provider metadata
distinguishes the last observed mutation-domain state from domains requiring a
new readback. A selected provider can submit `refresh_provider_observations` for
its established, unlocked domains and resources; the coordinator validates and
persists the complete batch atomically. This records the trusted provider's
native readback. It does not independently prove operating-system state.
Producer identity remains stable across consumer borrows; individual native
borrow tokens remain separate lease evidence.

## Processing and capture range

A selected GPU processor may require a capture range different from the client's
encoded output. The lifecycle coordinator obtains a read-only processing intent
from the selected provider using that provider's frozen settings and validates
the requested output before provider effects. `ProviderRequest.requested_video`
continues to describe the client output. When capture requires a different HDR
state, `source_video_mode` describes the source; width, height, and refresh must
match the output. It is omitted when the modes are the same.

The trusted composition identifies the processor and the source-mode-aware
participants. Only those explicitly selected participants receive the source
mode; capture owners in a managed composition must advertise
`source_video_mode/1`. A candidate must match the admitted source mode. Stream
preparation must preserve the session's processing enablement, processor, and
source range; changing them requires a new session. Providers must validate the
coordinator caller and admitted request as usual. A source mode is a requirement,
not proof of the captured pixels. With external capture, the selected processor
must independently reject frames whose format or color tagging is incompatible.

The version-one `processing_intent` response's `processor_settings` may enrich
the frozen settings with provider-owned context. Every frozen key and value,
including complete nested values, must remain unchanged. Consumers use the
public SDK `validate_processing_configuration` helper to check this and the
16 KiB configuration bound, and separately enforce exact target, revision,
selected processor and range requirements. They pass the validated enriched
configuration to the selected processor; substituting the input snapshot would
discard the provider's context. Added context grants no native authority.

## Composition protocol v2

The public SDK defines a separately negotiated composition protocol for an
owner that produces a resource and a dependent participant that configures it.
For the split display implementation, `nonary.virtual-screen/2` owns the virtual
resource and its native leases; `nonary.display-settings/2` supplies settings
policy. The protocol is generic: it does not require a display-specific shell or
expose an owner’s native lease credentials. Version 1 provider requests and
responses retain their existing form; a version 2 participant and its methods
must be explicitly selected and approved. Empty additive fields are omitted from
version 1 serialization.

The SDK contract and its structural validators are implemented. Broker and
fake-native tests exercise parts of the composed path; they do not establish
deployed-session or native hardware behavior.

The coordinator validates the selected participant dependency graph before
starting effects and persists the graph and the admitted snapshots. A producer
publishes a `ResourceOutput` identifying its service, capability, schema, origin
target and generation, resource identity and incarnation, execution domain,
revision, and complete mutation-domain observations. A dependent receives only
outputs selected from its declared dependencies. A session output can be used by
its child stream only after the coordinator verifies the exact persisted parent
and generation; structural validation alone is not that proof. Output data and
observations are evidence, not grants to control the resource.

A typical stream transition proceeds in this order:

1. The owner prepares or retains the session resource and publishes its staged
   output. Stream preparation does not open capture or take a capture borrow yet.
2. The dependent submits a read-only `ResourcePlan` with its
   `ResourceControlProposal`. The coordinator checks the installed method and
   schema approvals, exact output provenance, target, resource, domain state and
   requested rights, then durably admits the plan and its cleanup obligations.
3. The coordinator dispatches `resource_control` to the owner with a scoped,
   independently verified permit. The owner retains native authority, performs
   the admitted change, and returns its readback. An unknown effect stays fenced
   for recovery; a status query never repeats the effect.
4. An applied control result is provisional until settlement. The coordinator
   persists the exact request, output, and settlement ID, and calls the selected
   `resource_control_settle` method. The owner verifies that durable tuple with
   the coordinator and durably acknowledges it. Only an `Applied` acknowledgement
   of the same output, without a candidate, completes settlement. The dependent
   callback receives the frozen original inputs and confirmed control results.
5. The owner receives `resource_finalize_plan` through its registered
   `resource_plan` method. This read-only v2 request contains the current owner
   output and confirmed controls, but no admission. Its fresh plan accounts for
   the current revisions, rights, borrow and cleanup needs. The coordinator admits
   that plan before calling `resource_finalize`. The owner then takes the fresh
   capture borrow and returns the final output and, when a capture consumer needs
   one, its exact candidate binding. Capture opens only after finalization and the
   consumer acknowledges that binding. The standalone owner follows the same
   finalization stage even when no dependent control was selected.

On disconnect the coordinator fences work and retires stream consumers before
stream cleanup; the session resource remains retained for resume. Resume admits
a new stream and obtains fresh finalized binding evidence. End waits for
application retirement. Cleanup uses the persisted dependency graph, original
inputs, plans, and obligations in reverse order: a dependent's restoration must
settle before its owner releases or destroys the resource. An unknown or failed
dependent cleanup blocks release of its prerequisites and remains recoverable;
independent cleanup may continue. Timed retention requires an implementation and
qualification before it can be offered.

`SchemaCatalog` validates installed, digest-pinned document bytes against a
bounded, offline subset of JSON Schema; it is not a full JSON Schema engine.
Unsupported keywords and references are rejected. Numeric values in `const` or
`enum`, including nested values, are rejected until mathematical numeric equality
is supported. Schema equality and structural SDK checks do not substitute for
coordinator authorization or fresh owner observations.

The v2 action-target description is planning context. It can include a retained
owner output even when no stream is active. For removal, the coordinator checks
exact output provenance and a complete current borrower snapshot under the
admitted domain lock; missing unrelated streams in a description do not grant
permission to remove a resource.

## Settings and actions

Descriptors use namespaced IDs and declarative schemas. Adding an option or action
does not require a new central display-specific enum or management form. The
registry validates settings and applies compare-and-set revisions before saving.

When `settings_registry_service` is configured, the coordinator reads effective
provider settings before invoking providers. Session callbacks retain their
session snapshot; each newly admitted stream, including resume, receives a new
snapshot, which its release callbacks retain. Running streams are not silently
reconfigured by a settings save. Without that service setting, the installed
participant settings are the explicit static source.

Actions have typed arguments, allowed targets, required capabilities, a plan
method, an execution method, and optional status method. A mutating action first
retires affected consumers and acquires its admitted domain rights. The provider
claims one execution permit, then uses fresh nonce-bound `check_action_effect`
queries before subsequent native effects. Completion releases rights and retains
any outstanding cleanup obligations. The initial mutation implementation requires
a single active stream; unsupported target or borrower combinations are rejected.

The split SDK also defines a typed resource disposition for an action: a `release`
intent identifies the exact output, origin, resource, and resource generation.
The owner's result reports `released`, `retained`, or `unknown`. The coordinator
must authenticate the owner and admission and resolve dependent cleanup; the
typed result alone is not authorization or proof of retirement. A confirmed
release followed by resume requires a new resource generation rather than reuse
of the released generation.

`ActionResourceDispositionPlan.cleanup_obligation_ids` may nominate up to 128
sorted, unique IDs; an omitted or empty list claims no implicit cleanup. The
result's `cleanup_outcomes` may refer only to nominated IDs. The coordinator
matches full obligation tuples from the original root `session.prepare` plan
against the exact authenticated owner plan and completed receipt. A released
native resource alone does not discharge other cleanup obligations. Unclaimed,
pending, or unknown cleanup retains the root, admission, and rights and fences
unsafe conflicting reprepare. A compact work-proof reference points into private
history and grants no effect authority.

The `nonary.virtual-screen/remove-owned` action is restricted to an
active stream target. A session target, including a paused session without an
active stream, is unsupported. Its intended shortcut behavior is to retire the
stream and remove the owned producer while retaining the application; a later
resume must obtain a fresh resource generation.

When an invocation supplies `expected_settings_revision`, the action service
compares that decimal revision with the provider's current registry document
before planning. This is a point-in-time check: a concurrent settings save can
occur after the comparison, because admission does not reserve that revision.
Execution separately binds the selected session or stream's frozen settings
snapshot; the comparison does not replace that binding.

Keyboard actions run through the normal action service. The input module retains
press/release ownership, records delivery before committing an action, and uses a
lifecycle reservation. A transferred action may outlive its triggering input
lease; other input work must drain before display mutation. Ordinary nonkeyboard
input avoids the action journal. Shortcut discovery has a bounded cache; a newly
saved binding can take up to one second to become visible. A fresh input-module
boot epoch cannot reuse a previous instance's token.
The optional shortcut may be unset (`null`); when configured, its
`current_stream` target resolves to the triggering stream and must be permitted
by the action descriptor.

## Recovery and limits

Receipts are caller/initiator scoped. Repeating an admitted operation returns its
receipt; using its ID for different data is rejected. Completed target history
can be compacted into replay-blocking tombstones. Storage and tombstones are
bounded; exhaustion rejects admission rather than forgetting unresolved effects.
Replay and closed-stream scope fences have finite limits, including 8,192-entry
bounds. Exhaustion fences further admission; it does not make old operation IDs
safe to reuse.

### Ended holders

An admission holds its mutation domains until its provider confirms the work
it admitted. A holder whose owner has ended does not keep a later session
waiting on that indefinitely:

- An admission made by an earlier host run is displaced as soon as a later
  plan needs one of its domains, and is set aside before a plan is refused for
  admission capacity. Nothing of that run is still executing.
- An admission of this run whose session or stream was asked to end is waited
  for ten seconds and then displaced, provided trusted composition granted its
  provider `amaterasu.lifecycle/domain-takeover`. A provider with that grant
  stops changing a domain for an earlier admission once a request arrives
  under a newer one, and reports the earlier work's outcome as it ended.
- Without the grant the plan waits the full 20 seconds and fails with the
  domain and holder named, for example `mutation domain is reserved:
  amaterasu.frame-limiter/policy is held by ended session ... whose provider
  cannot hand it over`.
- A holder whose owner is still open is never displaced.

A displaced admission stays on record with its plan and cleanup obligations.
It holds no domain and counts against no bound, and the rights check before
every callback refuses it, so nothing more is dispatched under it. A late
outcome closes its own record and cannot release, refresh or publish anything
of its successor. `docs/lifecycle-admission-and-retirement.md` describes the
same rule across the other lifecycle layers.

Restart creates a new host epoch and fences old live bindings. Explicit
`recover_session` can query a known, persisted provider receipt using its original
resource identity under fresh owner authorization. It never replays an unknown
preparation. Missing receipts, uncertain native effects, or unconfirmed retirement
remain `recovery_required`; that state is not successful restoration.

Owner abandonment after restart covers an owner that kept its application
records only in memory and so can never report an earlier application retired.
Its `snapshot` lists each session's `host_epoch`, so the owner can find them.
The owner sends `recover_session` with `restart_abandonment: true` and a
current-epoch session target, so no grant beyond `recover_session` is needed.
It is accepted only for that owner's `recovery_required` session from an older
epoch when every stream is retired, no input, action, shared-source or
composition state remains, every retained admission is settled and owned by the
session, and the only retained work is the undispatched `end_session` release.
Otherwise it is refused, with `restart_effect_unknown` for dispatched, blocked
or uncertain release work. On acceptance the old end receipt is rejected as
`superseded_by_restart_abandonment`, and the frozen release graph runs its
normal provider cleanup under recovery authorization. The application is
recorded as `untracked_after_restart`: `application_retired` stays false, no
retirement proof is created, and closed-session certificates still require one.
A released session ends with `application: "untracked_after_restart"`; rejected,
deferred or uncertain cleanup returns it to `recovery_required`. Abandonment
lets bounded retention reclaim the session. It is not successful restoration and
does not prove that the application stopped. NVHTTP sweeps for such sessions at
startup and after a `session_capacity_exhausted` refusal; it never reports them
retired.

The `lookup_control_baseline` operation lets only the selected
virtual-screen owner read an owner-acknowledged original configure settlement
while its baseline remains pinned. The request contains `operation`,
`session_target`, `resource_generation`, the full authenticated `root_output`,
`operation_id`, and `request_digest`. The digest is lowercase SHA-256 over
`serde_json::to_vec` of the original typed `ResourceControlRequest`. A matching
response contains `session_target`, `target`, `root_output`,
`resource_generation`, and `settlement` with its `request`, `output`, and
`settlement_id`; other requests receive a generic rejection. The serialized
response is limited to 16 KiB. This read-only lookup grants no effect authority;
the endpoint and its integrated recovery path require qualification.

The `lookup_stream_closure` operation provides read-only historical
evidence that an exact owner control belongs to a closed stream. Its request is
`{operation: "lookup_stream_closure", target, resource_generation, resource_id,
incarnation, operation_id, request_digest}`. The target is the exact stream
lifecycle target, the resource generation is positive, and the digest is lowercase
SHA-256 of `serde_json::to_vec` of the exact typed `ResourceControlRequest`.
Only the configured and selected owner may query it. A verified result echoes the
identity fields with `status: "closed"`; failures return `status: "rejected"`
and `reason: "stream_closure_unavailable"`.
An optional `require_retirement_safe: true` requests the additional conservative
old-scope check; a successful response then includes `retirement_safe: true`.
This checks coordinator-side unresolved work and does not attest that the owner
has drained its own local state. Without the request the field is omitted.

The live proof checks the release graph and the owner's acknowledged settlement;
the archived path checks the selected checkpoint against current policy and
schemas. Neither result grants native effect authority. Archive retention and
garbage collection remain under review; finite capacity must fence admission
rather than silently discard necessary evidence. The endpoint and integrated
recovery path remain subject to qualification.

A failed or rejected preparation callback does not establish that its effects
were absent or cleaned up. Its admission remains held for recovery, until a
later session needs one of its mutation domains and the holder's owner has
ended; see "Ended holders" below. After a
restart, failed initial application preparation with no admitted application or
stream can query the selected provider's exact saved receipt. Only a cleanup
response confirming every obligation in the original plan as released, with no
materialized outputs or outstanding authority references, releases that
reservation. Missing receipts, deferred restoration, and unconfirmed effects
remain fenced. Providers must establish native retirement before publishing
this proof; rejection alone never qualifies. Confirming a resize after an
interrupted stream preparation does not publish a new stream or release that
preparation's rights.

## Validation

Run the focused suites with:

```sh
cargo test -p amaterasu-session-lifecycle -p amaterasu-extension-registry \
  -p amaterasu-extension-actions -p amaterasu-input
cargo clippy -p amaterasu-session-lifecycle -p amaterasu-extension-registry \
  -p amaterasu-extension-actions -p amaterasu-input --all-targets -- -D warnings
```

Opt-in tests additionally exercise independently built module ABIs through the
real broker. Their native display, capture, input, and state peers may be
simulated; consult each test's environment requirements and the display plugin's
README. These tests do not establish live display, Windows driver, network-client,
or hardware behavior. Linux fake-native and real-broker tests are separate from
hardware qualification. Windows virtual-screen owner startup is fenced while
its DACL and process-lock security remain incomplete. Building the public plugin
repositories currently requires a compatible private Amaterasu SDK checkout.

An unpatched Linux run through the real broker and state service with simulated
native and capture peers covered ten alternating 720p90 and 1080p60 cycles,
active and paused session end, and a second session in the same mutation domain.
A separate real-broker run with simulated native, state, and input peers covered
two keyboard removals, fresh resource generations and resume through generation
three, session end, and an exact retry after a lost completion reply without
native replay or injected input. These runs do not establish live hardware or
full host-restart behavior.

## Consumer-owned and provider-owned settings

The host routes lifecycle requests and checks grants. A participant reads the
authorized transition through the SDK and owns its option definitions,
validation, policy and effects. A consumer must not reconstruct a participant's
settings or select its backend.

Older product, application and device records can contain values whose provider
has moved to the extension registry. Readers retain those values as bounded
opaque data. Consumer edits preserve them without interpreting their meaning;
new plugin settings are edited through the registry descriptor. Importing an
older value into a plugin scope is a provider-owned migration, not an implicit
consumer fallback. Retained data does not confer permission to read a record
or apply an effect.

The management frontend renders provider descriptors with its own typed editors;
providers do not supply HTML or JavaScript. The optional `slider` presentation
hint selects a paired range and number editor for a bounded numeric definition.
The current frontend falls back to the typed editor for an unrecognized hint;
an unsupported definition still fails validation. Frontends released before
slider support reject this hint, so deploy a compatible frontend with a host or
provider that publishes it. Descriptor admission itself must understand the
hint before it can be published.

The coordinator can select `journal_in_runtime_state: true` instead of an
absolute `journal_path`. This requires the existing
`amaterasu.host-state-context/1` grant and places its journal in the worker's
private `session-lifecycle` directory. Selecting both locations, omitting both,
or failing to obtain the granted runtime context prevents startup; the module
does not fall back to a process-wide or temporary path.
