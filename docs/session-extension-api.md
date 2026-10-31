# Session extension API

Amaterasu provides session lifecycle, extension settings, and action services as
independently built managed libraries. Display behavior belongs to the separate
`amaterasu-display-settings` repository. The shell loads approved components and
routes their broker calls; it does not contain display policy.

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
