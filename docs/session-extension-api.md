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
| `nonary.display-settings/1` | Independently installed display settings and provider callbacks. |

Use [the opt-in local profile](session-extensions-local-profile.md) to compose a
package. Components, registered method schemas, callers, and grants must agree.
Native modules are trusted code; broker grants are not a native-code sandbox.
Neither the test profiles nor a successful package verification deploy a service.

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

Plans identify owned resources, mutation domains, expected revisions, affected
consumers, cleanup obligations, and an optional capture constraint. The provider
rechecks native state immediately before effects. The coordinator holds admission
rights until the callback settles, including across asynchronous status polling.
Native capture evidence is provider-specific; the coordinator preserves it and
checks the common negotiated mode and exact binding identity.

### Dependent-provider composition

The planned split uses separately approved `nonary.virtual-screen/2` and
`nonary.display-settings/2` services, built independently in the
[virtual-screen](https://github.com/nonary-streams/amaterasu-virtual-screen) and
[display-settings](https://github.com/nonary-streams/amaterasu-display-settings)
repositories. Virtual-screen owns the session-retained producer and its native
leases; display-settings consumes its selected output and applies configuration
for the stream. A resumed stream receives a fresh configuration while the
session's producer may be retained. The existing v1 provider contracts remain
unchanged. The split SDK and coordinator contracts exist in source; the current
single-provider profile does not implement this composition, and the complete
provider, cleanup, and action paths still require integration and qualification.

The session-owned output has a resource generation distinct from the session
and stream generations. Dependent requests bind to the selected producer output
and its resource generation; a stale generation cannot authorize effects on a
replacement. Disconnect retires the stream and its configuration while retaining
the application and eligible session resource. Session end additionally requires
application retirement and release of the root resource after its dependents.
Cleanup and action wiring for this split must be qualified before relying on
these semantics in a running host.

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
The owner service `nonary.virtual-screen/2` is intended to expose
`resource_control`, `resource_finalize`, and `resource_status` methods.

Release and rollback proceed in reverse dependency order. Independent cleanup
continues after a failure, but an unresolved dependent effect or cleanup must
fence release of its prerequisite. Recovery queries recorded operations rather
than replaying unknown effects. The selected graph, settings snapshots, and
inputs must remain stable for recovery; there is no silent provider substitution
after effects. These requirements need integrated tests and separate live
platform and hardware qualification.

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

The SDK contract and its structural validators are implemented. The composed
runtime path is **not yet qualified**: a successful end-to-end broker harness and
appropriate native adapter qualification are still required before claiming that
this sequence works in a deployed session.

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
of the released generation. End-to-end action release and new-generation resume
remain subject to wiring and qualification.

The planned `nonary.virtual-screen/remove-owned` action is restricted to an
active stream target. A session target, including a paused session without an
active stream, is unsupported. Its intended shortcut behavior is to retire the
stream and remove the owned producer while retaining the application; a later
resume must obtain a fresh resource generation. The target restriction and this
end-to-end path require integration and validation before use.

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

## Recovery and limits

Receipts are caller/initiator scoped. Repeating an admitted operation returns its
receipt; using its ID for different data is rejected. Completed target history
can be compacted into replay-blocking tombstones. Storage and tombstones are
bounded; exhaustion rejects admission rather than forgetting unresolved effects.

Restart creates a new host epoch and fences old live bindings. Explicit
`recover_session` can query a known, persisted provider receipt using its original
resource identity under fresh owner authorization. It never replays an unknown
preparation. Missing receipts, uncertain native effects, or unconfirmed retirement
remain `recovery_required`; that state is not successful restoration.

A failed or rejected preparation callback does not establish that its effects
were absent or cleaned up. Its admission remains held for recovery. A known
partial preparation cannot currently settle as a clean failure; that requires
explicit enrollment of its scoped cleanup obligations. For example, confirming
a resize after an interrupted stream preparation does not publish a new stream
or release the preparation's rights.

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
or hardware behavior.
