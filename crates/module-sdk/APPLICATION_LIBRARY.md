# Application library SDK

`application_library` provides JSON payloads for the `amaterasu.library/1`
read facade and the existing `amaterasu.catalog/1` provider snapshot actions.
Submit them using `RuntimeClient::request_serialized` with the appropriate
service and grants. The broker authenticates `IncomingRequest.caller` and
`initiator`; an application payload cannot select either. Startup configuration
and the host's signed component selection determine which implementation and
import principals are available. Native modules are trusted code; API grants
route calls and are not a native-code sandbox.

Follow the exact returned list cursor until null, even after an empty page.
Cursors bind a collection revision and can become stale after a mutation. The
facade's list and resolve operations expose the exported catalog view; they do
not grant provider mutation authority. Launch and lifecycle operations remain on
`amaterasu.launch/1`; this SDK does not imply lifecycle delegation to the facade.

A provider snapshot uses begin, ordered batches, then finish. The catalog maps
the authenticated caller to an owner at startup. It issues the generation and
collection revisions; forward the returned values and sequences exactly. The
batch validator checks unique source IDs, at most eight total entry/removal
intents, and at most 12,000 encoded bytes for the four batch arrays. Catalog entry normalization can increase the encoded size, so a locally valid
batch may still be rejected. The catalog also validates entry data, authorization
and concurrency. By default batches are committed incrementally, so failure or
an incomplete finish does not roll them back. An opt-in staged begin requests
atomic publication; require an explicit `staged:true` acknowledgement and
respect the selected catalog's storage and publication bounds. Incomplete staged
finish does not publish. Cleanup is requested only by a complete finish and its
explicit removal flags.
Never report an incomplete scan as complete.

Broker admission is not proof that a lifecycle effect occurred. On an uncertain
admission reply retry the identical payload with the same broker operation ID;
use the operation's own lookup/reconciliation protocol before issuing new work.
Keep its lifecycle operation identity stable independently of broker request
IDs. Do not infer successful launch, termination, or cleanup from a timeout.

Artwork references are provider-owned and opaque. Existing Steam and Lutris
artwork reads use provider-specific revision-pinned chunk protocols; the SDK
also defines provider discovery, artwork and lifecycle domain types and a
broker-facing provider adapter. Host-side selection and routing require
explicit configuration; lifecycle operations are not routed by the launch
coordinator.
The artwork validator pins chunks to a reference, revision, MIME, length, and
offset; the consumer must retain these values across the stream. Do not treat a
reference as a local path or assume a generic provider can serve it.

See `examples/application_library.rs` for payload construction. It is a local
serialization example, not evidence of a running host or installed provider.
