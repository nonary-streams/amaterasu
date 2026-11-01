# Launch option projection: evidence and integration boundary

Baseline: `743d8bb5837e7480a26333d4e734f957d85e803d`.

## Feature evidence

[Umbra's NVHTTP implementation at ab0c4d6](https://github.com/PreceptorOfMagic/Umbra/blob/ab0c4d6557fa248cd2274b963ca3d8d4a62f3cb3/src/nvhttp.cpp#L2877-L2895) reads independent per-stream hints from launch arguments. Its comments describe using a hint on both launch and resume. These are evidence of a need for per-stream intent, not a request to implement those features or reproduce their wire names in Amaterasu.

The built-in `modules/nvhttp/src/wire/streaming.rs` applies a fixed `LAUNCH_FIELDS` allowlist before constructing a closed `LaunchRequest`. `Query::parse` rejects duplicate field names and bounds the target and number of fields. Both launch and resume use that parser in `wire.rs`. At that baseline a separately composed participant could not add a field to the built-in protocol owner. A replacement protocol service could implement the feature, so this is a composition limitation, not an absolute inability of native plugins.

`launch/resume_snapshot.rs` constructs a new candidate `Record` from the incoming request and applies it only after validation. This is an existing per-stream generation boundary. RTSP's `Operation` uses `deny_unknown_fields`; merely attaching data to its `open` payload is not a working projection and may break compatibility.

## Proposed contract and decisions

The protocol owner parses its own wire format. Approved participants may interpret bounded input and produce versioned, namespaced option payloads; trusted composition, not client data, determines which participants may contribute and which selected consumer receives each namespace. Namespaces and versions must be explicitly approved against the selected consumer. A namespace is an identifier, never a provider selector or grant. Native code is trusted and these controls are not a sandbox.

Reject duplicate claims, incompatible versions, conflicting projections, unsupported required options, invalid participant replies, and excess count or encoded size before the associated application or stream admission effects. Existing history reservation and read-only preflight may occur earlier. Unknown optional input may only be ignored if the protocol explicitly defines it as optional; otherwise reject it. The chosen bounds must cover both aggregate and individual payloads and account for the existing request-target and broker message limits. The protocol participant owns schema interpretation; core must not encode feature-specific semantics.

The accepted result is immutable for a stream generation. Launch stores its validated snapshot with the request; resume constructs and validates a fresh snapshot and replaces stream-specific state only when the existing resume transition succeeds. Absence preserves existing behavior and existing clients require no new fields. No implicit carry-forward of a previous generation's client intent. Cancellation, authorization rechecks, deadlines and teardown must follow the existing wire job and session lifecycle.

## Historical integration dependency

At the baseline, the separately assigned protocol-dispatch work needed to specify how an authenticated built-in launch path invokes approved participants and binds their results to trusted selected recipients. At the baseline, `Router` only queued ordinary `Command::Request` jobs and its `Purpose::service` selected fixed services; the broker had owner-scoped configured hooks and API approvals, but NVHTTP did not invoke them for launch participants. The local integration candidate below uses the shared approved API call mechanism. The SDK snapshot helper takes candidates and an exact namespace/version approval predicate supplied by its trusted caller. It rejects duplicates, unapproved versions, invalid names, more than 16 entries, an entry over 4096 encoded bytes, or a collection over 8192 encoded bytes. Its private storage is exposed read-only. The initial SDK-only snapshot stored in the per-generation request did not itself accept participant wire fields or bind an approved recipient; the integration below adds those boundaries.

The integration touched these anticipated areas: `modules/nvhttp/src/wire/{query,streaming}.rs`, `modules/nvhttp/src/wire.rs` and its job resolution, `modules/nvhttp/src/launch.rs` and resume snapshot, and a shared SDK contract plus the actual selected consumer's existing request contract. The shared dispatch design determines the exact message shape. Overlap with the protocol-dispatch worker is `wire.rs` job admission and completion.

Validation must exercise an unchanged consumer with two neutral unrelated namespaces, absence, duplicate or conflicting claims, unknown required namespace/version, oversized input and result, authorization revocation, failed participant, launch and resume snapshots, and a replacement participant. It must also establish legacy default behavior and fail-closed operation when no participant is approved.

## Local integration candidate

`launch_participants` startup settings bind each participant's `CallApi` service, method and interface version to its permitted query field names, entitlement, namespace, option version and pinned output schema. Core launch fields cannot be claimed. The separately configured consumer service and schema pin must match `amaterasu.rtsp/1` and the output pin exactly. Only explicitly present claimed fields are sent, and participant arguments never contain the raw query or session keys. Participant replies are correlated with the original binding and validated before being added to the snapshot. The runtime performs API approval and caller-grant checks; live entitlement checks occur while pending, through stream startup and on reply delivery. Known request IDs use the existing cancellation path; unknown admission is retained until its bounded deadline without retry after disconnect or revocation.

The broker fixes the selected provider for each service for its lifetime. The
selected stream service's declaration is checked against trusted configuration
before participant invocation. The built-in RTSP provider rejects nonempty
options. For option-bearing launches, the assembled stream-open payload is conservatively
counted after read-only preflight and before lifecycle or application effects,
with bounded placeholders for later lifecycle identifiers and the admission
token. Every launch retains the exact size check immediately before dispatch. A rejected resume restores
the previous option snapshot and entitlements.

An independently built neutral replacement fixture exercises the broker path
through an unchanged launch consumer. It tests two broker principals backed by
the same fixture binary, not two independent implementations. No fork-specific
behavior is implemented.

The selected stream service may respond to the read-only broker action
`launch_option_capabilities` with `{ "version": 1, "options": [...] }`. Each
entry supplies a namespace, version, and exact `SchemaRef` pin. The SDK
`OptionCapabilities` parser bounds and validates this declaration. NVHTTP asks
only when a client supplies a configured participant field and intersects the
reply with trusted startup bindings before invoking participants. The built-in
RTSP service declares an empty list. An absent or invalid declaration rejects
the option-bearing request; clients cannot select a provider. Broker service
registration remains fixed for its lifetime, so the replying service is also the
selected stream service. The check is point-in-time, not an effect fence.
