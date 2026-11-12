# Outbound contribution lifetime

Trusted startup selects the protocol authority, input service and approved
schema/version/control-type mappings. Only the selected authority that owns the
session may supply its frozen agreement; no agreement grants no contributions.
The selected input provider authenticates its caller and active lease, binds the
session and verifies its own current resource incarnation before returning a
contribution. The session checks the contribution's shape and session and
resolves its schema/version through the approved agreement. The consumer does
not independently inspect a provider's resource state. Native providers are
trusted code, not a sandbox.

The optional `ScopedFeedbackPeek` request is a separate SDK type, leaving the
original `InputRequest::FeedbackPeek` and exhaustive enum shape intact. A
provider returns versioned `OutboundFeedbackEvent` entries through the existing
peek/ack mailbox. Providers that reject the optional request leave the same
selected service available for legacy polling. Malformed, wrong-session and
unapproved optional entries are discarded and acknowledged; an invalidation
whose provider-side outcome is uncertain causes optional entries to be dropped
while legacy feedback continues.

## Bounds and queue ownership

An agreement contains at most 16 schema mappings; schema names and resource
identities are at most 96 bytes each. A contribution payload is at most 3,960
bytes. A peek returns at most 16 events, and the **complete serialized JSON
response**, including its envelope and all events, must fit the runtime broker's
16,384-byte limit. JSON encoding expands byte arrays, so limiting only the raw
payload is insufficient. `bounded_feedback_response` checks the whole response;
providers must page their mailbox and must not acknowledge entries they did not
return.

The session copies an approved contribution into its existing feedback FIFO,
which is bounded to 64 messages and 16 KiB of payload. Successful queue copying
transfers responsibility from the provider mailbox to the session; it is not a
transport send or peer delivery. Contributions use reliable channel 0. BUSY
retains the head for a later retry, while OK means only local transport
acceptance. The transport owns destinations, encryption and sequencing. Queue
pressure and stop behavior continue to apply to legacy feedback as well.

## Invalidation and teardown

Stop or known lease invalidation cancels unsent queued messages. The session
processes relevant invalidations it has already read before pumping affected
new contributions. Because the generic contract does not map resource names to
incoming controller invalidations, it conservatively discards **all** queued
contributions on such a change, including entries for other resource names.
Later unrelated input does not keep the invalidation barrier pending forever.
An asynchronous provider resource change can race an already admitted message,
as with existing feedback; the contract does not promise atomic revocation or
recall of data already accepted by the transport.

The generic contract does not activate production wire negotiation. See the
[assessment](outbound-contributions-assessment.md) and [neutral provider
fixture](../../fixtures/outbound-neutral/README.md) for test evidence and its
limits.
