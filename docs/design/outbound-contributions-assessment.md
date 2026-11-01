# Session outbound contribution assessment

## Historical motivation

The original assessment at `743d8bb5837e7480a26333d4e734f957d85e803d`
identified a substitutability gap: the selected input provider could produce
only the closed set of legacy feedback variants consumed by stream-session.
Vibepollo's controller protocols provided evidence for multiple outbound
schemas and resource-specific peer capabilities. That evidence did not request
implementing those payloads or translating Vibepollo code into Amaterasu.

The earlier assessment also identified peer negotiation as a missing source of
trusted agreement. That remains a limit of the real product path, but its
conclusion that the generic consumer and SDK contract were unimplemented is
superseded by the implementation described below.

## Implemented generic boundary

Trusted startup composition selects the input service, protocol authority and
approved namespaced schema/version/control-type mappings. The authority must
also own the session; its supplied agreement must be a subset of the startup
approval. Agreements reject duplicate schema/version pairs, colliding types
and reserved control types. An absent agreement denies optional contributions.
The existing peer protocol does not yet produce such an agreement.

The additive SDK contract provides `ScopedFeedbackPeek`,
`OutboundAgreement`, `OutboundContribution`, `OutboundFeedbackEvent` and a
full-response size helper. The original exhaustive `InputRequest` enum and
its legacy `FeedbackPeek` constructor retain their source shape. The selected
trusted provider binds the scoped request to its caller, active lease, session
and current provider-owned resource incarnation before exposing an event; the
session checks the returned session, shape and frozen agreement. A native
provider is trusted code, not code sandboxed by these grants. The built-in input
provider continues to emit legacy feedback; if it rejects the optional scoped
request, the consumer continues legacy polling of the same selected service.

The consumer copies approved payloads into the existing bounded, reliable
feedback queue. Copying is the point where the session takes responsibility
for the item; it is not transport acceptance or peer delivery. The existing
transport owns its destination, encryption and sequencing. See
[outbound contribution lifetime](outbound-contributions-lifetime.md) for
bounds, invalidation and loss semantics.

## Evidence and limits

The neutral provider fixture is separately built against the public SDK and
loaded through the runtime and module ABIs. Its test uses runtime-created
caller endpoints, the runtime broker, the unchanged session consumer, the real
feedback queue and an in-process synchronous transport. It covers two approved
schemas, a denied mapping and BUSY retry. Other session tests cover invalidation,
legacy progress and stop. The surrounding session dependencies and parent
request in the loaded-module test are simulated. Instructions and artifact
paths are in [the fixture README](../../fixtures/outbound-neutral/README.md).

This establishes an optional generic boundary, not a complete implementation of
any fork feature. It does not qualify a signed host package, an installed host,
real peer negotiation, remote delivery or hardware behavior. No production wire
negotiation was activated, and no new peer capability is implied by the fixture.
