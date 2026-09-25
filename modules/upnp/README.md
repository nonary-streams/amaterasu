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
could race between those requests.

`discover_gateway` applies the crate's two-second search timeout and returns
discovery errors. Mapping and cleanup errors are also returned to the caller.
Tests use a deterministic fake gateway and do not contact a router.

This slice does not integrate configuration storage, settings migration,
management editing, WAN exposure policy, listener startup, or periodic mapping
refresh. `global:upnp` remains pending until those integration and parity gates
are completed.
