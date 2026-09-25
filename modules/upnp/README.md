# UPnP listener mapping lifecycle

This crate provides a caller-driven UPnP lifecycle slice. The caller supplies
the configured active listener endpoints to `MappingManager::reconcile`; the
manager skips inactive, zero-port, and IPv6 endpoints and requests matching TCP
and UDP mappings for each remaining IPv4 endpoint. External and internal ports
are the same.

Reconcile creates every replacement before removing stale mappings. If an add
fails, mappings created during that attempt are rolled back and the old owned
mappings remain. Existing mappings are never adopted or intentionally replaced.
Changing an endpoint's IPv4 address while keeping the same external port and
protocol cannot be staged on IGD, so reconcile reports the existing mapping and
leaves it intact.

Call `MappingManager::shutdown` during orderly shutdown. It removes only
mappings the manager successfully created whose full recorded mapping still
matches. Modified mappings are preserved. IGD exposes inspection and deletion as
separate operations, so the real adapter checks immediately before deletion;
the protocol does not provide an atomic compare-and-delete operation.
Likewise, creation preflights the mapping table before `AddPortMapping`, but
IGD does not provide an atomic create-if-absent operation; an external writer
could race between those requests. Refresh also checks the complete mapping
immediately before `AddPortMapping`; IGD provides no atomic compare-and-refresh
operation, so an external writer could race between the check and refresh.

Call `MappingManager::refresh_owned` periodically to reassert owned mappings.
It refreshes only mappings whose complete recorded value still matches, drops
ownership for missing or changed mappings, and preserves changed gateway entries.
Refresh errors are returned and their mappings remain owned for a later retry.
The current lease request is zero, which IGDv2 interprets as its maximum lease
of 604,800 seconds (one week). IGDv2 reports the remaining lease and counts it
down, so that dynamic value is excluded from stable ownership comparisons. The
mapping must be refreshed before that lease expires. The recommended
caller-driven interval `RECOMMENDED_REFRESH_INTERVAL` (30 minutes) is a safety
margin before expiry, not a timer; callers schedule refreshes. The crate does
not add a runtime dependency.

`discover_gateway` applies the crate's two-second search timeout and returns
discovery errors. Mapping, cleanup, and refresh errors are returned to the
caller.
Tests use a deterministic fake gateway and do not contact a router.

This slice does not integrate configuration storage, settings migration,
management editing, WAN exposure policy, or listener startup. `global:upnp`
remains pending until those integration and parity gates are completed.
