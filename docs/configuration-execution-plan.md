# Vibeshine configuration parity execution plan

Sol owns integration, cross-module contracts, security review, and
final verification. Luna XHigh workers take bounded domains with disjoint file
ownership. Each worker uses the pinned Vibeshine commit in
[configuration-parity.md](configuration-parity.md) as a behavioral reference,
without copying or translating its implementation.

## Definition of complete

For every stable ID in [configuration-coverage.json](configuration-coverage.json),
record the reference default, accepted values, validation and normalization,
platform conditions, persistence and migration behavior, every runtime consumer,
management authority, frontend edit behavior, and a meaningful regression test.
Mark an ID `verified` only after these match Vibeshine. A stored value or an
editor control alone is incomplete. Verify legacy aliases and every supported
application/client override independently. Keep genuinely unsupported behavior
`pending` and name the missing consumer or platform gate.

## Work sequence

| Phase | Luna XHigh assignment | Sol integration gate |
| --- | --- | --- |
| 1. Reference contract | Inventory parser, guide, web, integration, application and override settings; pin sources and defaults | Audit stable IDs, conflicts and omissions; run the inventory drift check in CI |
| 2. Settings foundation | Extend typed schema, caller-owned product documents, versioned updates and editor controls | Review authority, migration, CAS ambiguity, result release and concurrent request progress |
| 3. Small live slices | Assign one settings family per worker, including every relevant runtime consumer | Signed management test, restart persistence, frontend typecheck and domain regression |
| 4. Core streaming | Network, security, video, audio, input, capture, display and encoder families | Platform-specific negotiation and live-stream behavior; avoid storage-only parity claims |
| 5. Integrations | Steam, Playnite, Lutris, frame limiter, update, tray and diagnostics families | Provider reconciliation, launch/session behavior and platform defaults |
| 6. Application overrides | Per-application fields and all shared per-client override keys | Precedence, alias migration, launch effects, multi-client isolation and conflict tests |
| 7. Final audit | Independent Luna review of each remaining `pending` ID and every documented source mismatch | Zero pending IDs, complete platform gates, full locked suite, strict Clippy, rustfmt and signed host tests |

Workers should report concrete changed files, test commands and results, and
unimplemented behavior. Sol integrates only when the ownership boundary is clear
and the option's observable effect is tested. The current increment establishes
the inventory and settings foundation, host and application lifecycle controls,
session renewal, network port behavior, and Steam provider reconciliation. It
does not pass the final parity gate.

Each worker commits its owned implementation after rustfmt, meaningful tests,
and Clippy. Use the repository's Nonary Git identity and include these exact
lines in every plan commit, including integration commits:

```text
Orchestrated By: GPT-6 Sol High
Planned By: GPT-6 Astra High
```

Steam catalog reconciliation now owns explicit exclusions, app type selection,
recent-play selection, and the policy for retaining stale entries in all-game
sync. It reads local app metadata and play history before changing the catalog;
an incomplete scan leaves the prior catalog intact. Artwork, richer reference
metadata, and platform-specific discovery and launch behavior still need
verification. The Steam IDs remain pending until those effects are covered.

The global base port now derives HTTP, HTTPS, RTSP, and streaming UDP ports.
IPv4 and IPv6 listeners can coexist where the startup policy requests both.
`global:port` remains pending until listener moves, startup recovery, and
platform behavior are verified as one configuration contract. Durable live
updates of `address_family` and `bind_address` are in progress; those IDs
remain pending until every public listener follows their effective policy
without weakening the loopback control plane.

Per-application process lifecycle fields now preserve omitted values and apply
explicit values at launch. The shared `config-overrides` map and named-client
overrides remain pending: launch has no authenticated TLS client identity or
trusted client profile lookup yet. A request-supplied client ID cannot provide
that authority.
