# Authenticated duplex transfer boundary

Vibeshine's USB/IP controller transport is a behavioral example of an ongoing bidirectional exchange. Its device protocol is not implemented here. A trusted plugin could replace a complete network service; it could not participate in an existing NVHTTP connection because the consumer owned TLS and always closed after its HTTP response. The connection owner now has an optional generic upgrade path using two existing `TransferApi` channels, not socket or TLS-key transfer.

## Trusted selection and activation

An entry in the existing `protocol_handlers` startup configuration may opt into a generic upgrade with an `upgrade` object containing `protocol`, `input_format`, `output_format`, and `metadata_schema`. The latter three are nonzero versioned data-plane `Schema` objects. The protocol is a bounded ASCII token. The exact configured method/path must be distinct from every other participant and reserved built-in path. An upgrade candidate must use HTTP/1.1 with `Connection: Upgrade` and the configured `Upgrade` token. The default has no upgrade entries; ordinary response and pipelining behavior remains unchanged.

For example, a trusted startup configuration can include:

```json
{
  "path": "/neutral", "http_method": "GET",
  "service": "org.example.handler/1", "method": "open",
  "interface_major": 1, "interface_minor": 0,
  "schema": "org.example.request/1", "entitlement": "org.example/stream",
  "upgrade": {
    "protocol": "neutral",
    "input_format": {"namespace": 1, "value": 1, "version": 1},
    "output_format": {"namespace": 1, "value": 2, "version": 1},
    "metadata_schema": {"namespace": 1, "value": 3, "version": 1}
  }
}
```

The schema identifiers above are illustrative; the handler and owner must agree their actual meanings, including that the metadata schema is empty. The installed API approval must describe an `UpgradeOffer` request and an `UpgradeDecision` response. For example, successful acceptance is `{"outcome":"accept","version":1}`. A handler must keep both native bindings alive until its work has stopped; returning acceptance does not transfer a socket or certify delivery.

The configured handler's installed `ApiMethodApproval` selects its provider principal. NVHTTP queries that approval, verifies the TLS peer and named entitlement, reserves a SOURCE/SINK pair for that principal, and calls the already-approved method with a version-1 `protocol_handler::UpgradeOffer`. The offer contains the ordinary verified request envelope, transfer service, both internal IDs and their exact descriptions. These IDs are not credentials, and the remote HTTP client never receives them. The provider's signed policy must explicitly grant native use of `amaterasu.nvhttp.duplex/1`; NVHTTP needs native-provide permission for that service. Grants route trusted native code and are not a sandbox. The registered factory and host-owned channels live through connection teardown; consumer libraries must stop before the provider is unloaded.

The handler's distinct `UpgradeDecision` must be exactly version 1 and `accept` or `reject`. Unknown, malformed and ordinary HTTP responses fail closed. Only acceptance activates the pending connection after another peer/entitlement check. A denied, disconnected, timed-out or failed/uncertain handoff retires both channels; the existing broker operation identity and cancellation rules are reused. A handler effect already begun cannot be undone by cancelling its broker call. The 16 KiB broker limit applies to the whole wrapped offer, not each field independently.

## Data, ordering and lifetime

SOURCE carries verified connection plaintext to the selected plugin; SINK carries plugin output back. Each record is an ordered byte slice, not a protocol message. Both descriptions use independently configured payload schemas and an owner-approved empty-metadata schema; metadata bytes are disallowed. The host policy fixes 16 KiB chunks, 64 KiB queued bytes and four queued chunks per direction, and reserves at most 4 MiB across 64 channels. Backpressure retains producer ownership on rejection and pauses socket reads; already decrypted plaintext is drained before a new encrypted batch. An initial suffix larger than one chunk fails and closes the unupgraded connection.

The connection owner keeps the TLS state and socket. It retains plaintext after the parsed request ahead of later TLS input, and only acknowledges the upgrade response after rustls ciphertext has drained to the local socket. Plugin output cannot overtake that response. Output chunks are staged within the negotiated bound before partial writes; local queue admission and socket writes do not establish peer receipt.

Input EOF drains already accepted plaintext, finishes SOURCE and leaves output usable. Output finish drains SINK and sends TLS close notification; the connection retires after both directions finish and accepted input is consumed. Raw TCP EOF without `close_notify` also ends input after draining accepted plaintext; it does not prove an authenticated complete application transcript, so a protocol requiring truncation detection must define its own terminal record. Timeout, authorization loss, fatal I/O and cancellation discard pending data and synchronously retire both directions. The owner signals both channels before retirement barriers, unblocks external waits before joining workers, and never drops the pair from a receive callback.

Entitlement checks are point-in-time. The owner checks while pending and before each bounded TLS or channel effect, and serializes an observed denial with connection teardown. An authority change can race a successful check and the following effect; no atomic revocation or remote-delivery guarantee is claimed. A native consumer releasing either binding is detected on the owner tick and stops both directions.

## Validation and limits

`cargo test --offline --locked -p amaterasu-module-sdk -p amaterasu-nvhttp --lib` covers the ordinary route and upgrade contracts. `python3 scripts/test_duplex_consumers.py` builds two neutral C consumers and runs the same adapter and loopback TLS owner against each. The TLS test uses the production client-possession verifier, pins a public test server certificate and verifies handshake signatures; it uses a controlled authority callback and test broker. The runtime test separately checks native principal enforcement. These tests do not establish remote network performance, an installed feature, or real paired-client behavior with a deployed plugin. Explicit raw-EOF, pending-EOF, rejection and lost-admission scenarios are exercised by the corresponding `AMATERASU_DUPLEX_*` test runs.
