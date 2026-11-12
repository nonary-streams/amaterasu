# Recovered extension boundaries

The recovery preserves the public launch-option snapshot validator and generic
bounded duplex channel/connection adapter from the November 1 SDK candidate.
They are SDK facilities, with no production HTTP upgrade or launch routing added.
Current built-in session, artwork, authentication and provider-owned retained
options continue through their existing contracts.

Launch options are immutable bounded snapshots validated against exact trusted
namespace/version bindings, digest-pinned schemas and the selected consumer's
declared capabilities. These helpers authenticate nobody and select no provider.

`data_plane::DuplexPair` reserves two runtime-attested channels atomically, signals
both directions before retirement, and returns their budgets after callbacks
finish. `data_plane::connection_owner::DuplexOwner` preserves bounded input/output
and partial-write progress. A connection owner establishes authentication and
composition and fences effects on observed denial. EOF or local admission is not
remote delivery. Construction, use and teardown belong to an owning worker;
Drop can wait for in-flight callbacks and must never run on the shared host tick.

The original HTTP upgrade prototype destroys this adapter from its polling path.
It needs worker-owned teardown before integration. The original launch-routing
prototype rewrites workflow paths since replaced by main's capture composition,
retirement and presentation work. Those integrations remain preserved in the
original candidate instead of replacing current production code.

Validation covers the SDK's two-direction retirement, bounded queues and generic
adapter, plus two independently compiled C consumers bound through the current
runtime. It does not qualify TLS upgrade routing, peer feature negotiation,
installed packages or a production client feature. No Apollo policy is included.
