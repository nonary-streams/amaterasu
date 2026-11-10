# Approved publisher repositories

## Development workflow

Amaterasu's native release unit is a complete component set with one runtime and up to 64 feature libraries. The host verifies it before loading, stages a content-addressed copy, and trials it in a disposable worker at restart. UI assets are an independently signed web bundle selected without replacing the native set. The Windows service launches the worker in the active user's session. There is no general elevated broker yet.

The development administrator CLI supports `repository-approve`, `repository-revoke`, `repository-refresh`, `repository-list`, `repository-download`, `repository-install-ui`, `repository-stage-native`, `repository-activate-native`, and `repository-restore-ui`. Run `amaterasu-release` without arguments for exact syntax. These CLI mutations require a development bootstrap. The management console's **Plugins** page lists the modules the host loaded, the choices its repositories offer and the repositories themselves, and lets a signed-in administrator add, refresh and remove repositories on any host (see [Managing repositories from the console](#managing-repositories-from-the-console)). The management-owned `/recovery` page remains available independently of the selected UI. The fixed host shell also owns `web-recovery-status`, `web-recover`, and `native-recover ROOT [DIGEST]` commands. A specific retained, previously healthy digest can be recovered if the previous publisher was revoked; a merely staged set cannot be recovered directly to active. These recovery commands do not load selected native code; mutations are development-only until installed-service authorization and protected state exist. Native download and staging verify bytes without loading them. Only `repository-activate-native ROOT DIGEST` explicitly authorizes a future worker trial; a subsequent development `amaterasu probe ROOT` or worker startup executes that publisher's native code and promotes the set only if healthy.

Approval requires an ID, HTTPS location, Ed25519 public key, and its lowercase SHA-256 fingerprint confirmed through a separate trusted channel. The URL is only a byte location. Approval lets the publisher supply **any native component slot, including management or the runtime, and the UI**. This is broad publisher authority, not an add-on-only or cosmetic permission. A signature establishes provenance, not benign behavior or elevation. Native code shares the worker's user token and can use ordinary user process facilities; runtime grants are routing permissions, not a native code sandbox.

`catalog.json` is compact canonical JSON matching `repository::Catalog`'s field order. `catalog.sig` contains a lowercase hex Ed25519 signature over `amaterasu.repository.catalog.v1\n` followed by those exact JSON bytes. The catalog binds repository ID, OS/architecture target, expiry, increasing sequence, and each `(slot, implementation, version, kind, manifest_sha256)` choice. The signed manifest then binds each native artifact or UI asset hash and size. A candidate is accepted only under its approved publisher key. HTTPS retrieval refuses redirects and bounds headers and bodies. Repository files are at `catalog.json`, `catalog.sig`, and `{native|web}/{implementation}/{version}/manifest.{json|sig}` plus manifest-listed payload files. These paths are a transport convention, not a trust anchor.

Approval, revocation, the latest verified catalog, and its replay checkpoint are stored together under `repositories/approved.json` with an atomic replacement record. A repository ID cannot be reused after revocation; rotation currently requires explicit approval of a new ID and revocation of the old. UI selection keeps a signed release sequence/digest floor per publisher and for the project source. Development native selection also records the repository ID, pinned fingerprint and highest accepted native sequence/digest per publisher key in the atomic host selection record. A lower sequence or a different digest at an accepted sequence cannot be staged. Normal activation requires the current native floor and a still-listed, unexpired catalog choice; explicit recovery can select a retained older signed set without lowering the floor. The worker rechecks the live approval and signature under the approval lock through library loading. Revocation prevents subsequent starts, but does not stop a worker already running that code. A revoked selected UI becomes unavailable until restoration. The existing `stage_bundle` entry point relies on its caller to pass only project bootstrap keys; the repository staging entry point additionally binds each fingerprint to its key bytes.

## Platform and privilege model

The intended distribution is one logical signed catalog and package contract with prebuilt target-specific native libraries and portable web assets. Windows uses DLLs, Linux shared objects, and later macOS dylibs. End users should not need Cargo; building with Cargo is a publisher/developer workflow. The existing in-process native boundary is a versioned C ABI, not Rust's unstable ABI. Its interface registry permits namespaced services beyond a fixed product feature list. Component dependencies currently require exact versions and must be resolved as a complete set before activation. A dropdown choice does not imply a safe live native hot swap: the host activates a set at a session/restart boundary and trials it before promotion.

Publisher identity and privilege remain separate dimensions. Ordinary native components belong in a user-token worker on Windows, a non-root process on Linux, and a standard user process on macOS. The host loader now rejects an elevated Windows token or Unix effective root before loading ordinary libraries. This is a basic elevation check, not full token/capability confinement; it does not inspect individual Windows privileges or Linux capabilities. Native libraries share their worker's address space and user privileges.

An installed bootstrap policy can approve arbitrary namespaced API methods such as `org.example.paint/1` with exact provider, callers, interface version, request and response shapes, and a user/elevated privilege declaration. The user-process runtime accepts a descriptor only from its approved provider endpoint, negotiates versions, and validates both request and response. Shape checks cover top-level fields and coarse JSON kinds; handlers must still validate nested data, ranges and operation authority. Raw service calls cannot bypass a method contract. Delegated calls require approval for both the immediate caller and the original initiator. The host binds an endpoint to each verified component ID; the command payload cannot choose a caller ID. This is cooperative routing policy for native code, not isolation from hostile code in the same process. Core `amaterasu.*` and internal `hooks/` services cannot be shadowed by a dynamic approval. Elevated method registration is rejected by this broker. A future elevated process needs authenticated IPC, separately installed handlers and identity-bound grants; signing a module must not self-authorize privileged calls. The broker must not expose arbitrary commands or generic run-as-admin.

Windows virtual input will use a separately approved VHF-backed device path with
a minimal HID source driver and a session-bound companion, as detailed in
[Windows virtual input boundary](windows-vhf-input.md). The current input backend
still uses SendInput for keyboard/mouse. The separate UMDF VHF driver and
companion device transport now build but are not installed, selected, or
connected to an authenticated service/grant route.

## Remaining work and release blockers

- Development native sets can be staged with publisher-bound provenance and a durable per-publisher floor, then explicitly authorized for trial at a worker lifecycle boundary. This accepts a complete set from one publisher; independent mixing of components from multiple publishers and production repository activation remain unavailable. Revoking an active publisher requires stopping its running worker and manually recovering a retained set from a still-trusted signer before the next service start. A UI already loaded in a browser keeps running after revocation and may retain an API session; production revocation must also close affected browser sessions and invalidate their API credentials. New asset responses check approval under the repository lock. The development root's ordinary same-user files are not an adversarial filesystem boundary against replacement between hash verification and OS library loading; production needs protected ownership/ACLs or an equivalent immutable image-loading strategy.
- Host recovery exists as development CLI commands outside replaceable modules, but a production recovery surface needs installed-service authorization, protected state and a usable route when the user worker fails. Recovery requires a retained signed set whose manifest has not expired and whose signer remains trusted; production needs a durable trusted recovery artifact or renewal plan. The management `/recovery` page alone cannot serve that role.
- Composition of independently sourced components needs a whole-set dependency solver, interface/API negotiation, cross-platform platform requirements (including Linux libc), and health-tested activation. The present target string covers OS and architecture only.
- Installed user-process API registration, method schemas and grant checks are implemented. Browser access to those methods, grant review UI, privileged handler installation and authenticated elevated broker IPC are not. The browser HTTP router has a fixed route table.
- The console approves, refreshes and revokes a publisher under an administrator's session, on production hosts too. Production still needs machine-protected state/ACLs, key compromise response, revocation distribution, a refresh/freeze policy with reliable time, and closing the browser sessions of a revoked interface publisher. The development CLI is not a production trust manager.
- The console does not install what a repository offers. An authenticated browser install flow needs a review of the grants a set's new components request, because the host activates only policy changes its protected allowlist names; that review and compatibility diagnostics are future work.

No real publisher key was enrolled, no downloaded third-party code was run, and no installed service permissions or persistent production trust were changed.

## Managing repositories from the console

The Plugins page reads `POST /v1/repositories/list` and `POST /v1/package-selection/status` with `configuration.read`. Three more routes change the approved repositories:

| Route | Body | Effect |
|---|---|---|
| `/v1/repositories/add` | `id`, `location`, `public_key`, `fingerprint` | Enrolls a publisher |
| `/v1/repositories/refresh` | `id` | Fetches and accepts the publisher's current catalog |
| `/v1/repositories/remove` | `id` | Revokes the publisher |

Each needs `configuration.write` and an administrator's own signed-in session. An API token is refused whatever its scopes, as it is for token and session management. The management module performs the change with its Network SDK provider on one worker thread; a second change is refused with `repository_busy` until the first finishes. The request returns an operation number at once, and the listing's `operation` field reports that change as `running`, `succeeded` or `failed` with a code until the next change starts.

Adding keeps the release tool's rule: the administrator supplies the public key **and** its fingerprint, and a fingerprint that does not belong to the key is refused before any network request. The console then fetches `catalog.json` and `catalog.sig` and records the approval only when that catalog verifies under the key for this host's target. A repository ID can never be enrolled twice, so a mistyped key or ID must not consume it. Enrolling lists a publisher's choices and adds its key to those that may sign a selected interface bundle. It does not download, stage or activate anything.

Removal is revocation and is permanent for the ID. It is refused with `repository_in_use` while the selection attributes the active, pending or trial native set to that repository, and with `repository_serves_console` while the selected interface bundle verifies only under that publisher's key. Either removal would stop the host or lock out its administrator at the next start.

The listing reports each repository's saved catalog as `current` (with its sequence and expiry), `expired`, `missing` or `unavailable`. A catalog that no longer verifies withholds that repository's choices only; the other repositories stay listed. `manageable` is false on a host whose management module has no network provider or web state, and absent on a host that predates these routes; the page then lists repositories without offering changes.

A publisher can hand out its details as one JSON object, which the Add dialog accepts when pasted into any field:

```json
{"id":"example-plugins","location":"https://plugins.example.org/amaterasu","public_key":"<64 hex characters>"}
```

The dialog fills in the ID, address and key from it. It never takes the fingerprint from pasted details: the administrator enters that separately, from a channel other than the repository address.

`POST /v1/modules/describe` with `{"id": "<component id>"}` returns one loaded component's declared dependencies, the components that require it, and the services it asked to provide and call, read from the running set's signed manifest. These are requests; host policy decides what is granted. Library file names are not returned.

## Including an independently built library in a local set

The development `pack-linux-session` command accepts an explicit
`--external-component TEMPLATE_JSON ARTIFACT` pair after its sequence number.
The publisher builds the external library separately, then supplies its reviewed
component manifest as `TEMPLATE_JSON` and the library bytes as `ARTIFACT`. The
manifest uses the ordinary `ComponentManifest` fields, including the identity,
version, output filename, ABI major, exact component dependencies, and requested
routing grants. Its `sha256` and `size` fields are placeholders: packaging
recomputes both from the selected artifact. The external component must not be
a runtime, and its identity and output filename must differ from the profile's
components.

Before packaging, the development root's bootstrap policy must separately
approve every requested grant for that exact component identity. Provider API
methods, lifecycle hooks, descriptor hashes, caller scopes, and component
settings also need their own explicit policy approvals where applicable; the
artifact or template cannot authorize them. The publisher signs the *entire*
component set with the local development key and verifies the resulting
signature, target, artifact hashes, identities, and dependency graph. Package
selection does not stage, activate, or load a library. The selected native code
is trusted in-process code, and broker grants are not a native-code sandbox.
This workflow does not mix independently signed sets or establish production
publisher trust.

When a selected library supplies a service that an existing component must call,
`--call-grants CALLS_JSON` may follow the external-component pair. The JSON is
an object mapping selected component IDs to arrays of service names, for example
`{"amaterasu.extension-registry":["example.display/1"]}`. Each service must
be provided by a component selected in the signed set and independently approved in
the installed bootstrap policy for each named caller. The publisher adds only
those calls to the signed component requests; an absent, unselected, or
unapproved caller or service is rejected. Add every required transit caller
explicitly. API method schemas and caller authorization still require their own
operator policy approval. For a worked
application-library provider binding and lifecycle limitations, see
[provider configuration](application-library-provider-configuration.md).
