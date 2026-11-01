# Bounded protocol handler boundary: assessment

Vibepollo `src/nvhttp.cpp` (`getClipboard` and `setClipboard`) illustrates a
pair of independently permitted GET and POST operations on an authenticated
connection. They also require a current stream for the verified client. The
existing Amaterasu broker can host a service, and trusted native code could
replace a whole protocol service, but the current NVHTTP consumer does not
forward such requests: `wire.rs` rejects POST and bodies before routing and
only knows fixed routes. This is a composition limitation, not proof that the
SDK cannot implement the feature at all. No clipboard implementation belongs
in this change.

The proposed consumer boundary is an exact, startup-composed method/path match
bound to one broker service, versioned request schema and named entitlement.
Duplicate claims and collisions with built-in routes must prevent startup;
unknown routes remain denied. Normal broker call grants still apply. Requests
carry the composition-selected schema, bounded method/target/body and verified
TLS peer certificate and address; the handler must trust the protocol owner and
broker provenance, since a serialized value cannot prove its own origin. Native
modules are trusted, not sandboxed. Handlers should not receive TLS keys or
socket ownership. Schema-specific validation remains the responsible handler's
job; generic structural validation does not establish semantic validity.

NVHTTP must check the selected identity authority before a call, during pending
work and immediately before delivery; denial, authority unavailability and
unknown entitlements fail closed. It owns response validation, deadline and
broker cancellation on expiration/disconnect. The 16 KiB broker payload limit
applies to the complete serialized message, so individual maximum fields do not
guarantee that their combination fits. No duplex upgrade is part of this work.

The baseline identity ABI has only fixed permission selectors. Another work
area is defining named entitlement checks; integration must use its final
interface and preserve compatibility rather than treating pairing or an
unrelated List/Launch permission as authorization. Stream ownership checks
remain a separate service concern and are not claimed by this boundary.
