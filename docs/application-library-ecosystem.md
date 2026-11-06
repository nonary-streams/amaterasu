# Application-library ecosystem and extension plan

Research snapshot: **2026-10-31**, Amaterasu
`f8aa56337288632df64db1cca807dd3b0cd6beaa`. Twelve Terra High agents each
reviewed one host. That research pass recorded requirements and an extension plan.
The subsequent, authorized [first implementation increment](application-library-interoperability.md)
adds an optional versioned read contract and opaque execution handoff. No third-party provider, installation,
game launch, client interoperability, or hardware behavior was qualified.

Amaterasu already has useful extension boundaries: independently built library
replacements, provider discovery/import, and opaque provider-owned launches.
Those boundaries can accommodate many additional launchers without adding their
store types to core. **The inspected bundled interfaces do not expose the full
feature set surveyed here.** This is a finding about those implementations and
their current contracts, not an architectural limit on replacement plugins.

## Architecture constraint: independently replaceable plugins

The user's intended architecture includes independently replaceable networking
and packet delivery as well as the library. Plan extensions as contracts between
selected plugins, not additions that solder the built-in Catalog, Launch,
management, NVHTTP or transport implementation together. The shell stays generic;
product behavior and its persistence belong in independently built libraries.

- A library owns its catalog, discovery, metadata, overlays and library policy.
  It can use internal source adapters or independent providers. It need not use
  the bundled Catalog or a fixed list of stores.
- Launch/session implementations own execution and resource coordination.
  They agree with the selected library on stable application references and
  authorization; an adapter can bridge to the existing Catalog-backed launch.
- Management and client-protocol plugins consume agreed library capabilities.
  A protocol adapter maps those to its own wire representation. NVHTTP's
  ID/name-only rows and numeric aliases are requirements of that implementation,
  not a universal library model.
- Network and packet-delivery plugins own their respective transport behavior.
  A library must not depend on their packet formats, pacing, retransmission or
  delivery machinery. Library identity and operations remain transport-neutral.
- Versioned services, explicit capabilities, approved routing and component
  lifecycle provide composition. Interoperability profiles can standardize useful
  agreements without making every plugin implement every product feature.

Library replacement selection is verified in the current development packaging
path. This audit does not establish that arbitrary replacement of all other
subsystems is already supported by shipped tooling. Networking/delivery
replaceability is recorded here as the required architectural direction, not a
claim of completed implementation or a new packet-delivery design.

## Survey and evidence

[This community host list](https://www.reddit.com/r/MoonlightStreaming/comments/1vr8jct/moonlight_hosts_clients_forks/),
[a second list](https://www.reddit.com/r/MoonlightStreaming/comments/1ulw7t6/a_small_list_of_hosts_clients/),
and [recent fork discussion](https://www.reddit.com/r/MoonlightStreaming/comments/1wzko2g/are_there_any_new_forks_besides_apollo_and_artemis/)
guided discovery. They establish community interest, not an exhaustive census or
quantitative popularity ranking. Project source and official documentation, not
Reddit performance claims, support the behavioral findings. Some additional
hosts were included because their library design exercises different boundaries.

| Host and detailed audit | Reference needs worth retaining | Evidence qualification |
| --- | --- | --- |
| [Sunshine](application-library-hosts/sunshine.md) | Manual applications; command/preparation/environment/cwd; process-less Desktop; covers; stable identity across indexed source edits. | Official docs/source; moving upstream revision, unpinned. |
| [Apollo](application-library-hosts/apollo.md) | App/client profiles and hooks, stable identities, ordering, client permissions and artwork. | Official docs/source; moving upstream revision, unpinned. Arbitrary per-app ACLs not established. |
| [Vibeshine](application-library-hosts/vibeshine.md) | Playnite/Steam/Lutris sync policies, categories/exclusions/recent expiry, source ownership, user edits and artwork. | Local Canary, behind tracked remote and dirty; public docs supplement it. |
| [Vibepollo](application-library-hosts/vibepollo.md) | Related sync policies; app/client configuration; UUID/order/artwork-cache projection and lifecycle. | Local Canary ahead of tracked remote with dirty submodules; published-release equivalence not established. |
| [Foundation Sunshine](application-library-hosts/foundation.md) | Desktop Steam/Epic/GOG/folder scan, preview/selected or add-all import, metadata enrichment and covers. | Official documents pinned to `204a96826606b3a34cf2cd813aed0f0a9bba7db0`; no runtime verification. |
| [Wolf](application-library-hosts/wolf.md) | Profile app sets, process/container runners, image provisioning, persistent app data and shared lobbies. | Official moving stable docs; exact implementation revision unpinned. |
| [DuoStream](application-library-hosts/duostream.md) | Independent Windows-user instances, client pairing and multiple Steam instances. | Official docs/release; implementation unavailable in inspected public repository. Separate catalogs and library API remain unknown. |
| [Rubylight / former Butterpollo](application-library-hosts/rubylight.md) | Installed multi-store library, host-local vs portable game identity, tri-state installed state, cover versions and permitted session actions. | Official docs pinned to `55decb95abfe49847cfa06ed2567fb7b0d913054`; full entitlement catalogs not inferred. |
| [Punktfunk](application-library-hosts/punktfunk.md) | Plugin sources, missing/nonlocal titles, source/user metadata precedence, artwork roles and asynchronous provisioning. | Official docs; code-level behavior and exact source revision unverified. |
| [Hermes](application-library-hosts/hermes.md) | Apollo-derived Linux apps/import, Steam/Gamescope launch context, display placement and client endpoints. | Official moving docs/source; exact revision unpinned. Per-app ACL extent unknown. |
| [Polaris](application-library-hosts/polaris.md) | Steam/Lutris/Heroic/ROM discovery, preview/import, readiness/ranking, artwork roles and enhanced client views. | Official moving docs/source; exact revision unpinned. Generic install/progress API not established. |
| [Moonshine](application-library-hosts/moonshine.md) | Manual argv/hooks and Steam, desktop, Lutris, Heroic and Bottles scanners. | Official README; exact implementation revision unpinned. Separate Rust host, not established as a Sunshine fork. |

The [machine-readable inventory](application-library-ecosystem.json) retains
canonical identities, evidence status, report paths and requirement associations.
Read each report with the [cross-host boundary review](application-library-hosts/boundary-review.md):
its "generic API gap" and "core gap" findings concern the inspected interfaces
and selected implementations, not a requirement to move behavior into the shell
or a claim that replacement plugins cannot implement a different contract.

Keep these additional references separate from the twelve completed audits:

- [SolarFlare](https://github.com/vindeckyy/Solar-Flare) is archived/EOL; its
  README names [Slipstream](https://github.com/vindeckyy/slipstream) as successor.
  Slipstream is a follow-up candidate, not a completed compatibility assessment.
- [ChildStream's announcement](https://www.reddit.com/r/MoonlightStreaming/comments/1wmzxsg/i_made_a_free_opensource_way_to_stream_a_second/)
  identifies a Windows session wrapper around Sunshine. Its library internals
  were not audited; multiseat interest alone does not establish new catalog fields.
- NVIDIA GameStream is a legacy protocol reference. Client forks and companion
  tools such as Nightjar, Nova, Playnite imports and MoonDeck can reveal consumer
  needs but are not counted as additional host audits.

## What is extensible today

The [provider guide](application-library-provider-configuration.md) documents
startup-selected routes, authenticated importer/source ownership, snapshot
publication and provider lifecycle. Store scanning, credentials, runner choice,
container/image implementation, source-specific filtering and process tracking
belong behind independently built providers. Core should not enumerate every
store or accept arbitrary provider paths and commands from clients.

A selected replacement for `amaterasu.library/1` can own its storage, metadata,
overlays, sync and artwork. NVHTTP already calls the selected library for
`artwork_open` and the binary-transfer SDK, so a conforming replacement can supply its own
images without changing Catalog's artwork enum. An approved management extension
can call replacement-specific services. These are implementation opportunities,
not features proved by the read-only demonstration replacement.

There are concrete integration constraints when retaining today's bundled
consumers and services:

- **Bundled catalog artwork is closed.** `ArtworkReference` admits only Steam,
  Lutris and Playnite (`modules/catalog/src/model.rs:94`), and bundled routing
  reflects that (`modules/library/src/artwork.rs:10`). The generic SDK artwork
  callback does not connect another provider to that path. A provider-neutral
  reference and route are needed for interoperable Catalog-backed artwork.
- **Legacy reads are strict.** NVHTTP accepts ID/name-only rows, canonical
  numeric-backed `app-` IDs in ascending order, matching revision/cursors, and
  no empty continuation pages (`modules/nvhttp/src/wire/applications.rs:19`).
  Extra rich fields cannot be added to `/1` in place. The SDK's empty-page
  guidance (`crates/module-sdk/src/application_library.rs:21`) differs from
  this consumer; a replacement must satisfy the actual consumer today.
- **The current library selector does not replace launch.** NVHTTP launches through Catalog and
  Launch, whose provider routes validate importer/source provenance. Private
  replacement rows need matching authorized Catalog launch records in that
  composition. A different launch/protocol plugin can agree on a different
  stable reference contract. A static demonstration row is not proof of either.
- **Existing management edits Catalog.** Replacement-specific workflows require
  an approved extension or a new common management integration; they do not
  appear automatically in the bundled editor.
- **Component identity is not device identity.** Broker caller/initiator checks
  establish service principals. They do not automatically carry an authenticated
  paired device, human, profile or seat through library/provider requests.
- **Durable catalog is not durable execution.** Provider lifecycle requires
  operation identity, reconciliation and fenced retirement, but Launch's own
  records and reconciliation are documented as in memory
  (`modules/launch/README.md:379`). Cross-restart execution recovery needs its
  own implementation and qualification.

## Tracked interoperability requirements

The rows below retain the full interoperability requirements; they are not
mandatory shell APIs. LIB-001 and LIB-003 now have a partial implementation in
the optional `/2` read/handoff profile. Broker and standalone-artifact tests do
not close the remaining acceptance requirements, including client-wire artwork,
edits and authenticated user/device scope. Each requirement belongs to the selected
library, provider, consumer or session plugin, or to an optional compatibility
adapter. P0 means establish the relevant boundary first; P1 is behavior negotiated
only by participating plugins that need it. Associations in the inventory are
planning links, not a claim that every host implements every part.

| ID | Priority and requirement | Minimum contract and ownership | Acceptance evidence needed |
| --- | --- | --- | --- |
| LIB-001 | P0: compatibility and version selection | Preserve `/1` where existing consumers are retained; use separately versioned services for participating plugins' richer needs. Select supported versions explicitly; capabilities never confer grants. Document current validators and launch binding as a compatibility profile. | Separately built plugins interoperate through agreed references; retaining NVHTTP additionally proves multipage legacy list, artwork and authorized launch IDs. Unsupported versions fail explicitly. |
| LIB-002 | P0: provider-neutral artwork interoperability | For the bundled Catalog-backed path, attach an opaque provider/source-scoped reference and revision to a fixed approved route. Keep replacement-owned artwork possible. Negotiate roles and user choices separately. | Two providers with colliding image IDs, stale/revoked references, interrupted reads, changed bytes/MIME/size, bounded transfer and release; actual client reads. No paths, URLs or service selection accepted as client authority. |
| LIB-003 | P0: rich safe reads and identity | Stable library identity, provider/source identity and optional portable store identity remain distinct. Participating consumers can negotiate source, description/tags, installed true/false/unknown, readiness/reason, optional art roles, capabilities and revision. Source-specific data stays implementation-owned; no universal Catalog schema is required. | Store duplicates and manual entries remain distinct; mutation/query/scope invalidate cursors consistently; unavailable facts remain unknown. Private commands, paths and credentials never enter the public projection. |
| LIB-004 | P0: ownership, user overlays and ordering | Separate source-owned fields from user metadata, cover choice, visibility and per-app settings. Define revision-checked edits, resync precedence, detach/remove/hide semantics and deterministic order. A replacement can own this database; shared edit services are needed only for common editors. | Rescans preserve user choices; incomplete scans do not remove unseen rows; conflicting edits report conflicts. Rename/reorder/restart preserve launch identity; portable IDs, numeric aliases and artwork cache versions are not conflated. |
| LIB-005 | P0 where required: authenticated scope and launch context | Host-issued context or host-side policy binds device/user/profile/seat as needed across list, resolve, art, actions, launch and control. Pass only provider-needed effective launch context; keep settings and resource ownership in their proper subsystem. | Two scopes with overlapping source IDs cannot cross-read/launch/control. Test forged context, revocation, stale cursors and direct ID guessing. Apply app/client profile and hook ordering once, with truthful cleanup. |
| LIB-006 | P1: source management and selected import | Optional discover/configure/rescan/preview/select capabilities with stable candidates, selection state and explicit complete/partial results. Provider owns scanning, source filters, auth and policy; common UI uses a negotiated service. | New Heroic/ROM/Bottles-style provider needs no core store enum. Selected/add-all import, duplicate/unsupported candidates, cancel, partial failure, idempotent retry and source withdrawal preserve ownership. |
| LIB-007 | P1: availability and asynchronous actions | Optional provision/install/update/uninstall actions, separately authorized. Stable operation identity, accepted/running/terminal/unknown states, bounded progress, supported cancellation and lookup/recovery. Provider owns downloads and affected files; launch requires confirmed readiness. | Lost acknowledgment and retry do not duplicate effects; cancel/restart/withdrawal races stay truthful; unauthorized actions are denied. Missing items are excluded from immediately playable legacy views unless safe behavior is implemented. |
| LIB-008 | P1: refresh and change reconciliation | Revisioned polling is sufficient where appropriate. A common change cursor/event contract, if added, defines retention/expiry, scope, missed updates and full requery. Push delivery is not assumed durable. | Reconnect after missed changes converges; scan outage differs from uninstall; art-only refresh does not terminate a game. Stale query/scope/revision cursors cannot mix snapshots. |
| HOST-001 | Separate plugin capability work: lifetime, restart and concurrency | Selected execution/session/media/input plugins distinguish disconnect/retained game/close, durable launch ownership, independent seats and shared-lobby participants. Library metadata or provider container support does not supply those effects. These are not shell product features. | Real plugins and clients prove stop/recovery after lost replies/crash, retained ownership while uncertain, two isolated sessions, and independent join/leave if offered. No session/hardware parity claim from this audit. |

## Proposed implementation sequence

1. **Define plugin interoperability profiles (LIB-001).** Separate transport-neutral
   library/application references and capabilities from any wire format. For the
   existing NVHTTP/Catalog-backed profile, reconcile SDK guidance with validators
   and demonstrate replacement-owned artwork/metadata with authorized launch
   records. Keep alternate library/launch/protocol compositions possible. Define
   version selection and migration before adding fields. Qualification should
   replace each side independently using at least two implementations or contract
   fixtures, without editing the other side or adding product behavior to the
   shell. Keep protocol aliases and packet-delivery details behind their owners.
2. **Make bundled artwork provider-neutral (LIB-002).** Carry importer/source
   ownership and revision through attachment, lookup, routed read and cleanup.
   Keep the existing client transfer format and approved fixed routes. Do not
   call this complete after adding a new SDK struct alone.
3. **Specify the smallest useful rich read/edit contract (LIB-003/004).** Start from
   proven consumer needs, bounded projections and explicit ownership/CAS. Keep
   provider-private data and management extensions viable. Participating plugins
   own the implementation; do not require storage in the bundled Catalog. Where
   retaining legacy clients, preserve aliases and define export changes separately.
4. **Integrate authenticated scope where needed (LIB-005).** Trace the real
   certificate/account/profile decision across every consumer and launch route.
   Existing broad permission checks remain distinct from per-entry entitlement.
   Do not infer arbitrary per-app ACLs from Apollo/Vibepollo or Hermes docs.
5. **Standardize optional management and operations (LIB-006/007/008).** Use Wolf
   and Punktfunk provisioning plus Rubylight's narrower documented actions as
   acceptance references. Installed-only providers can report unsupported.
   Require operation recovery and honest unknown results before automatic launch.
6. **Qualify participating plugin capabilities independently (HOST-001).** Reuse existing
   lifecycle and session-extension contracts where they fit. Restart safety,
   multiseat, co-op and proprietary client wire compatibility remain separately
   scoped work with real-provider/client evidence. A replacement transport or
   delivery implementation should not require edits to the library's data model.

The research plan did not select version numbers, service names or a final schema.
The [first implementation profile](application-library-interoperability.md) now
specifies the optional `/2` read/handoff boundary and its tested composition;
the other extensions still require producer/consumer design and qualification.
Changes to current Catalog, Launch, management or NVHTTP apply only where those
plugins participate; they are not mandatory dependencies of every future library
implementation. Do not publish unused DTOs as evidence of interoperability.

## Refresh procedure

Retain the twelve canonical host identities and aliases in the inventory. On a
future refresh, resolve the selected upstream branch/release and record an exact
revision when possible; preserve moving-doc and local-dirty qualifiers. Review
the actual shipped artifact for release/runtime questions. Revisit the watchlist
when new primary evidence reveals a distinct library need. Update a requirement's
status only with corresponding implementation and acceptance evidence; neither a
new SDK type nor a synthetic fixture closes end-to-end compatibility work.

Native components are trusted code. Grants, scoped routing and provider retirement
assertions are authority/contract mechanisms, not an OS sandbox or independently
verified proof that all effects stopped.
