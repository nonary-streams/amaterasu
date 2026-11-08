# Host composition for process-lifetime native providers

Status: proposed design, not an activated feature. See
`detached-process-reaper-design.md` for the process and output requirements.

The current host constructs one runtime per feature graph, and the runtime's
native registry and bindings are destroyed with that graph. A normal feature
provider is also unloaded with it. Registering an ordinary provider through
that registry therefore cannot by itself make a detached-child wait owner live
across a same-process graph reload. The current `detached-process` core is not a
native factory and does not yet implement the process API, managed process
containment, or shared output sink. It is not safe to compose it in production.

## Selection and authorization

Add a generic, explicit process-lifetime native-service approval to protected
bootstrap policy, rather than inventing an implicit host provider or granting
native authority to an unlisted pseudo-principal. The proposed optional
`RuntimeConfig` policy field maps each exact service alias to an approved
provider component ID and exact versioned interface-family ID. Missing means
no process-lifetime service is selected. It must be bounded and validated with
the existing service-name rules; duplicate providers or aliases are rejected.

`resolve_runtime_grants` must include an approval in the effective runtime
configuration only if the verified manifest contains exactly that provider,
its requested `native_provide` includes the service, and the installed policy
approves the same grant. Consumers must separately request and receive the
`native_use` grant, explicitly select the alias in trusted configuration, and
declare a compatible manifest dependency on that provider. A signed component
request alone must not grant authority or select an implementation. Selection
failure, a missing capability, or a mismatched interface identity is an error,
not permission to fall back to a local implementation. Grants are not a native
code sandbox.

This field belongs to protected policy, not package-controlled host context.
Preserve the existing default-empty behavior and approval/migration checks for
older packages; never insert a process service or a consumer grant on their
behalf. The generic field is needed because the host should compose a selected
native provider, not embed application-specific process behavior. The exact
schema and capability IDs must be finalized with the SDK ABI before any package
claims this service.

## Ownership and reload

Add a versioned provider-lifetime entry point alongside the ordinary module
ABI. Its provider-owned factory and state are created from the verified selected
artifact without retaining a graph endpoint. Each new runtime registers that
factory through the selected provider's authorized endpoint and the exact typed
native interface identity. Graph bindings, caller grants, and module dependencies
remain per-runtime; a binding must not outlive its runtime. Provider work must
never retain callbacks into an unloaded consumer or graph.

The host's process-wide native state owns the loaded provider library, factory,
and provider instance independently of `LoadedGraph`. Library and factory
destruction must follow the provider's explicit retirement proof, and must not
run while any worker can execute its code. The service implementation, including
process policy and reaping, belongs in an independently built library; the host
owns only verified selection, ABI/lifetime management, and activation gating.

Pin the verified component identity, ABI family, version and content digest,
not just its filename. The provider remains pinned when a graph using it is
destroyed. A new graph may bind that same verified provider when its approved
selection and digest match. If an approved update selects different provider
bytes while old children, sinks, operations or bindings remain, either retain
the old verified provider as a separate bounded instance until it proves
retirement, or reject/defer activation. Never redirect opaque objects to the new
instance, unload the old one, or disable signature, policy or service-pin checks
to permit the update. A replacement with no active resources may retire the old
instance and load the newly verified artifact. Bound both provider instances
and all provider resources; define which condition opens the activation barrier.

## Work required before wiring

- Finalize and version the factory entry point and SDK contract, including
  provider shutdown/retirement, caller-scoped tokens, cancellation, idempotency,
  capabilities, resource limits, and synchronous versus worker-backed calls.
- Implement the complete selected provider behind that contract, including
  shared output-sink identity and managed/preparation process behavior. A
  detached-only process registry cannot replace the current application provider
  without changing output, elevation or containment semantics.
- Add the policy schema, effective-grant filtering, manifest dependency and
  package activation compatibility checks, and update the release composition.
- Migrate the application consumer through the SDK and test it unchanged against
  two independently implemented providers. Preserve current callers' API and
  behavior, and remove any temporary graph-local fallback before qualification.
- Test real runtime binding across graph A teardown and graph B startup in one
  host process. Cover wrong/missing grants, absent dependencies, contract/size
  mismatch, stale token use, resource exhaustion, exact artifact pinning,
  rejected or deferred replacement, concurrent managed-tree ownership, and
  successful detached child reaping after graph A has been unloaded.

The host's existing quarantine on incomplete graph shutdown remains in force.
An integration fixture, an allocated registry, or a passing SDK replacement
test alone is not evidence that a running installation uses this design.
