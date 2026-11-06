# Session presentation SDK

`amaterasu_module_sdk::session_presentation` is the version 1 contract for a
selected plugin that presents a client-specific application list and admits
application-independent streams. A protocol adapter supplies authenticated
client facts and an observed application snapshot. The plugin owns synthetic
entry identities, visibility, confirmation and retained-session policy. An
unchanged adapter can use another implementation of the same contract.

The service is `nonary.session-presentation/1`, API major 1, minor 0. Applications
and selections use opaque string identities. Numeric conversion, XML and other
protocol details belong to the adapter. No fixed Remote Monitor or Remote Input
identities are built into the SDK.

## Selecting and registering a provider

Trusted package composition selects the provider, approves the exact schemas
returned by `session_presentation::methods()`, grants callers, and declares
startup dependencies. The provider registers each approved method:

```rust
for method in amaterasu_module_sdk::session_presentation::methods() {
    runtime.require_success(Command::RegisterApi { method })?;
}
```

Use `session_presentation::command(RequestOptions { .. }, &request)` to build a
`Command::CallApi`. The helper validates bounded values and uses the exact
versioned method. A provider decodes the delivered API envelope with
`session_presentation::decode(&service, payload)` and authorizes the runtime's
caller identity before acting. Raw operations on this API service are rejected.
The adapter validates replies with `decode_reply` and checks that the variant
matches its current operation before taking effects.

The runtime authenticates module principals. The adapter must separately derive
`Client.id` from its authenticated transport and read current view, launch and
terminate permissions. A query-string client identifier is not that authority.
Native plugins remain trusted code; these grants do not sandbox native code.

Amaterasu's pairing authority grants permissions independently per certificate.
Its supplied client labels are not unique: two approved certificates may share
one label and have different permissions. The built-in adapter therefore uses
the authenticated certificate fingerprint consistently for game ownership,
retained roles and layouts. A rotated or newly paired certificate is a distinct
identity. Cross-certificate continuity requires a future explicit association
owned by the pairing authority; labels or protocol `uniqueid` values cannot
establish it.

An explicitly selected provider's absence, incompatible schema or rejected
request is an error. Consumers must not silently select a built-in policy.

## Bounded catalog projection

The broker limits a payload to 16 KiB. The SDK keeps each request and reply below
12,000 serialized bytes and limits a page to 32 entries. A row's opaque properties
may consume much of a page, so split by encoded bytes as well as row count.

1. `BeginProject` freezes the authenticated `Context` and complete catalog
   revision, returning an opaque `projection_id`.
2. `AppendProject` supplies the full inventory in bounded pages. Include hidden
   rows, retained aliases and profile metadata so they still occupy identities.
   Mark only the final page with `final_page: true`.
3. `ReadProject` reads the finished presentation in bounded pages, using the
   returned `next_offset` unchanged.
4. `ReleaseProject` retires the transaction after delivery or abandonment.

Providers bind a transaction to the broker caller and client. They cap retained
transactions, entries and memory, expire abandoned work, and refuse reads before
the complete inventory arrives. The SDK defines four concurrent transactions,
16,384 source entries and a 30-second transaction lifetime as bounds.

Adapters preserve the snapshot across collection and delivery. A changed catalog,
game generation, owner or client permission invalidates the pending view. Release
transactions on timeout, cancellation, connection loss and revoked trust. A
projected or cached entry does not authorize a launch: dispatch must revalidate
its original target against the current complete catalog.

`Info` supplies a small per-client running-application presentation without
transferring the catalog. `Artwork` resolves either a current application artwork
reference or a bounded inline PNG. Resolve application references through the
selected library under current authorization.

## Admitting streams and preserving source lifetime

`Dispatch` accepts a client-scoped `operation_id`, intent and optional selection
and mode. A provider may return `Pending`; poll with the same operation identity
and unchanged arguments. A different payload must not reuse the transition.
`CancelDispatch` retires provisional work when the adapter abandons admission.
After authority-confirmed unpairing, `RevokeClient` stops new admissions and
retires that exact client's retained roles and reservations. It remains pending
until ownership and native cleanup are verified; an idle retained source does
not require the caller to invent a live session tuple.

The decision describes an application launch, resume, replacement, stop,
auxiliary stream or completed control. A replacement or stop includes the
observed application generation. The consumer must compare it with current state
before acting, so a delayed control cannot stop a successor application.
`Auxiliary` requires Launch permission to create a role; `ResumeAuxiliary`
requires View permission for an already retained role. A synthetic Resume tile
may arrive through a protocol's launch route, so the HTTP path cannot determine
this semantic permission. Recheck the required live permission before effects
and delivery.

An `AuxiliaryStreamPlan` requests a selected synthetic black source or a verified
exact output. It contains an opaque session, generation, admission reservation,
mode and audio requirements. A normal application can borrow a retained output
through `SharedCapture`, which freezes the same source and mode. Neither type
contains native providers, OS handles or native allocation ownership.
`Game.source`, when present, is the trusted coordinator's original application
source association. The provider checks that original owner's source and game
generation before issuing a secondary client a fresh independent reservation.
An attach must not reuse another client's reservation unchanged.

Before any source-consuming effects:

1. The policy provider retains the admission reservation before returning a plan.
2. The trusted consumer verifies a borrowed source with `VerifySource` and checks
   that the returned source exactly matches the requested owner, generation,
   reservation, mode and selector.
3. The consumer reads the transport owner's runtime epoch, then calls
   `BindTransport` with the exact future transport operation and epoch.
4. The policy provider acknowledges only after persisting that binding and its
   source hold. The consumer opens the transport with that expected owner epoch.

An application source has a longer lifetime than a transport. Before application
or lifecycle effects, `BindApplication` persists the original source reservation
with the execution provider's exact application operation, authoritative launch
epoch and root application generation. Keep that source through a transport
pause, and allocate separate reservations for resumed or joined transports. The
original source hold retires only after actual application cleanup or the
execution provider's authenticated application-retirement proof. Retiring an old
RTSP operation does not prove that its application no longer needs the output.

Native capture still resolves the logical exact selector and checks readiness.
An unavailable target must fail rather than capture another display. A borrowed
source must not trigger unrelated display reconfiguration. Other required
lifecycle providers still run; trusted composition explicitly authorizes the
coordinator's frozen borrowed-source attestation.

Deliver `Observation` values with exact session, generation, reservation,
transport operation and runtime epoch. `StreamStarted` follows actual startup.
`StreamEnded` follows native retirement, and `FailedStart` releases the exact
provisional reservation. Retire broker results and retry uncertain observations
without retargeting their identities.
`Request::from(observation)` preserves the complete observation when forwarding
it through the API adapter.

`ExplicitDisconnect` applies to the auxiliary role. A normal application can
continue holding an independently admitted reservation on the same output. The
provider drops the role while retaining the source until its final borrower
retires. A 410-style control completion is emitted after actual stop and cleanup;
admission alone is not completion.

Crash recovery verifies the recorded transport operation and epoch through the
transport owner's authenticated retirement status. A restart alone, an empty
current-session list, or an unreconciled new epoch does not prove old work retired.

## Replacement validation

`crates/runtime/src/session_presentation_tests.rs` exercises the unchanged SDK
consumer through the real broker against two different selected principals.
It checks full schema and version identity, grants, no implicit fallback,
caller-owned cancellation and results, invalid response rejection, and repeated
result consumption beyond the broker's retained-request capacity.

Those tests establish the control contract. Platform display mutation, native
capture, input injection, signed package activation and real client playback
require their own qualification.
