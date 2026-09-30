# UPnP listener mapping lifecycle

This crate provides an owned UPnP mapping lifecycle. The caller supplies live
listener endpoints to `MappingManager::reconcile` or protocol-specific endpoints
to `MappingManager::reconcile_protocols`; inactive, zero-port, and IPv6 endpoints
are skipped. External and internal ports are the same.

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
The requested lease is one hour, and
`RECOMMENDED_REFRESH_INTERVAL` is 120 seconds. IGDv2 reports the remaining lease
and counts it down, so that dynamic value is excluded from stable ownership
comparisons. The crate exposes the interval but does not run a timer; its caller
must schedule refreshes.

`discover_gateway` applies the crate's two-second search timeout and returns
discovery errors. Mapping, cleanup, and refresh errors are returned to the
caller.
Tests use a deterministic fake gateway and do not contact a router. The current
adapter manages IPv4 port mappings; it does not open IPv6 firewall pinholes.

Mapping ownership is deliberately stricter than the pinned implementation:
pre-existing or externally changed conflicts are preserved instead of deleted.
The IGD protocol still has check-then-act races around create, refresh, and
delete; the adapter documents those limits at each operation.
