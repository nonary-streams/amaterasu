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

## Shared participant invocation for protocol and launch

Use the existing installed `ApiMethodApproval` and `Command::CallApi` for each
participant: the runtime binds the exact service, provider principal, method,
interface major/minor, callers, and coarse schemas, and enforces call grants.
The protocol owner's trusted startup configuration binds a wire operation to
that approved method and its required entitlement. The remote client never
selects a service, method, entitlement, recipient, or schema. Duplicate
method/path claims and built-in route collisions fail startup. Unknown requests
remain denied. A typed broker approval is not an entitlement and is not a
native-code sandbox.

Launch can invoke multiple independently approved methods through this same
boundary. Trusted composition additionally binds each participant's output
namespace and version to the already selected consumer. Launch must check
those bindings and validate all replies before effects; the HTTP response
envelope is not a launch-option schema. No separate hook or router is needed.
The existing selected-consumer contract and launch snapshot own projection and
lifetime; defining or implementing that projection is outside this change.

The earlier single-service wording above is refined here: the `CallApi` method
is the selected participant, as authorized by the broker approval, rather than
an untyped service call. The existing hook registry is not suitable for this
request path without further changes: ordinary hooks have no lost-acceptance
lookup, and idempotent `RunHookOperation` identities are retained for the
broker lifetime with a limit of 512. The request broker instead retains normal
operation identities temporarily; retries must use identical operation data,
recover the accepted ID, and cancel/release it on timeout or disconnect. A
cancel request cannot undo a handler effect that has already begun.

Identity commit `d8d955b5` adds ABI 1.2 `check_entitlement(certificate, name)`.
Only an OK status authorizes; the caller independently verifies TLS possession
and binds each operation to the entitlement. NVHTTP must recheck at admission,
while pending, and at reply delivery. This branch is based on the original
baseline and requires integration with that commit before using the new table.
