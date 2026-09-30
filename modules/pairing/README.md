# Moonlight pairing authority

`amaterasu-pairing` implements the Moonlight/GameStream Gen7 PIN-pairing
cryptographic transcript behind `amaterasu.pairing/1`. It is a native module,
not an HTTP or TLS server. The pinned Vibeshine host behavior at
`aa292c4cb097f22beab19d6ebf0307367f9bf449` and Moonlight Qt's client transcript
were used as behavioral references, without copying their implementation.
No real Moonlight application has yet qualified this module end to end.

Settings default to empty principal sets:

```json
{"administrator_principals":["amaterasu.console"],"transport_principals":["amaterasu.nvhttp"]}
```

Both lists allow at most 16 runtime principals. The module needs `provide` for
`amaterasu.pairing/1` and `call` for `amaterasu.state/1`. Callers need pairing
call permission. State requests preserve their runtime parent; the current
runtime delegation policy also requires the initiating caller to have state
call permission. State itself namespaces data by the immediate provider identity.
Never grant callers access by trusting a payload caller or client identifier.
Native modules are trusted code; runtime grants are not a native-code sandbox.

An administrator first initializes a persistent RSA-2048 host key, self-signed
X.509 certificate, and random UUID:

```json
{"action":"initialize"}
{"action":"identity"}
```

Initialize is idempotent after persistence, returning existing public identity.
Both successful operations return `ok`, `host_id`, `certificate_pem`, and
`certificate_id` (SHA-256 of certificate DER). No public response returns the
private key. Initial generation can take significant CPU time inside a tick;
it is an administrator setup operation, not an untrusted pairing request.
Generated certificates use SHA-256 with RSA PKCS#1 v1.5 and a 20-year validity.
The key is stored as unencrypted PKCS#8 PEM in the state namespace, requiring
host/state filesystem protection; this is not a keystore or encrypted-key vault.
All signing uses randomized RSA blinding. RSA operations use RustCrypto RSA and
an OS-seeded RustCrypto ChaCha20 RNG; entropy failure aborts before starting.
There is no RSA decryption endpoint.

Administrators may instead import an existing valid identity before initialization:

```json
{"action":"import_identity","host_id":"12345678-1234-4234-8234-123456789abc","certificate_pem":"...","private_key_pem":"..."}
{"action":"import_client","client_id":"old-moonlight-id","name":"Migrated device","certificate_pem":"..."}
```

`host_id` is optional on import and otherwise newly generated. Identity import
validates the certificate's self-signature, validity window, RSA parameters, and
matching private key. It cannot overwrite an initialized host. Client import is
an explicit administrator trust decision, bypassing PIN approval while still
validating the certificate. It requires an initialized host and the same durable
acknowledgment as pairing. PKCS#1-only private keys, encrypted key files, SHA-1
certificate signatures, non-RSA keys, and CA chains are unsupported. Supported
certificates are self-signed RSA 2048–4096 bits using SHA-256/PKCS#1 v1.5.

The following service operations form the actual Gen7 pairing transcript. Byte
fields are hex strings; the HTTP adapter translates Moonlight's
hex-encoded PEM `clientcert` to `client_certificate_pem` when beginning:

```json
{"action":"begin","client_id":"moonlight-uniqueid","name":"Living room","client_certificate_pem":"...","salt":"<16 bytes hex>"}
{"action":"approve_pin","pairing_id":"<returned id>","pin":"1234"}
{"action":"get_server_cert","pairing_id":"<returned id>"}
{"action":"client_challenge","pairing_id":"<returned id>","clientchallenge":"<16 bytes encrypted hex>"}
{"action":"server_challenge_response","pairing_id":"<returned id>","serverchallengeresp":"<32 bytes encrypted hex>"}
{"action":"client_pairing_secret","pairing_id":"<returned id>","clientpairingsecret":"<16-byte secret plus RSA signature hex>"}
```

Begin returns `ok`, `pending:true`, opaque random `pairing_id`, and `expires_at`
(Unix seconds). Only a configured administrator runtime caller can approve the
four-digit PIN. The PIN is not returned or stored, and cannot be injected by
adding fields to a public phase. Pending transcripts are bound to the initiating
runtime caller. An adapter serving multiple clients must also keep a private
mapping between its HTTP request/client and the returned opaque pairing ID.
An attacker-supplied Moonlight `uniqueid` is metadata, not proof of possession.

Get-server-cert returns `pending:true,paired:0` while waiting for approval.
These internal pending replies must **not** be forwarded to Moonlight: the
HTTP adapter must hold the original getservercert request open and poll this
service until approval. After approval it returns `ok:true,paired:1,plaincert`
(hex-encoded host certificate PEM). Successful challenge phases return `paired:1`
with wire fields `challengeresponse` (48 encrypted bytes) and `pairingsecret`
(16 secret bytes plus host RSA signature), respectively. The final phase returns
`paired:1` and `certificate_id` only after certificate persistence succeeds.

The transcript is:

1. AES key = first 16 bytes of SHA-256(salt16 || UTF-8 PIN).
2. AES-128-ECB decrypt the client's 16-byte random challenge, without padding.
   Return encrypted SHA-256(client challenge || raw host certificate signature ||
   random server secret16) followed by random server challenge16.
3. Decrypt the client's 32-byte hash commitment. Return server secret16 followed
   by RSA PKCS#1 v1.5/SHA-256 signature of that secret.
4. Verify the client's signature over its secret and constant-time compare its
   commitment with SHA-256(server challenge || raw client certificate signature ||
   client secret16). Both checks must pass before storing the certificate.

AES-ECB here is mandated by the pairing wire protocol and is not offered as a
general encryption facility. Gen6/SHA-1 pairing is not implemented; the
adapter must advertise a Gen7-or-newer host version. Certificate DER signatures
and secret signatures are actually verified, not merely parsed.

Each successful phase advances once. Out-of-order/replayed phases consume the
owned transcript and fail closed; a foreign caller cannot consume it. Wire-size
validation failures are rejected before advancement. Every transcript expires
120 seconds after begin, including PIN waiting. There are at most eight pending
transcripts, one per client ID, and eight admitted begins per global 60-second
window. The bounded ledger resets on module restart. Failed certificate parsing
consumes an admitted begin attempt. Restart discards all pending transcripts.

Administrative and trusted-transport operations:

```json
{"action":"pending"}
{"action":"cancel","pairing_id":"..."}
{"action":"list_clients"}
{"action":"unpair","certificate_id":"<SHA256 certificate DER>"}
{"action":"check_client_certificate","certificate_pem":"..."}
```

Pending and list-clients expose bounded metadata only and require an administrator
caller. Cancel requires the initiating caller; unpair is administrator-only and
idempotent. Unpair also cancels pending transcripts for that fingerprint.
The store holds at most eight paired certificates, additionally bounded by a
12,000-byte document including host identity, so large certificates may reach
the byte cap sooner. Expired paired certificates remain listable/removable but
cannot authenticate. PEM certificate and key inputs are each at most 4096 bytes;
requests are at most 12,000 serialized bytes. Client IDs are 1–64 identifier bytes
and names 1–128 bytes without control characters.

Check-client-certificate is allowed only for `transport_principals`; it validates
the supplied certificate and looks up its persisted fingerprint, returning
`paired:1`, `certificate_id`, and client-ID metadata on success. This operation
is **not** proof that a caller holds the private key. The trusted adapter must
first obtain the certificate from a successfully authenticated TLS handshake
with proof of private-key possession. This lookup is the authority check for
Moonlight's final HTTPS `phrase=pairchallenge` once a TLS adapter exists.
Client ID alone never authorizes subsequent streaming. There is currently no
TLS handshake, private-key handoff/signing interface for TLS, HTTPS pairchallenge
route, XML adapter, certificate-revocation push to active connections, or stream
lease integration in this module.

All durable operations use revision-checked state writes and wait for state
acknowledgment before successful completion. Verification reloads persisted state,
so acknowledged unpairing does not depend on a stale authorization cache. State
transport result tracking is released with the SDK's ephemeral-result helper.
The pipeline checks that the parent request is active before applying a loaded
operation. Cancellation can still race a subsequent mutation: a completed phase
may be consumed or a final pairing may be committed even when response delivery
fails. Clients must restart a lost transcript and administrators can inspect and
unpair committed certificates; the service does not fabricate durable idempotency.
Transport and state errors are reduced to fixed codes without secret text.

Replies on failure use `{"ok":false,"paired":0,"code":"..."}`. Codes include
`invalid_request`, `invalid_service`, `forbidden`, `not_initialized`,
`already_initialized`, `invalid_identity`, `invalid_certificate`, `identity_failure`,
`entropy_unavailable`, `signature_failure`, `pairing_failed`, `invalid_phase`,
`pairing_not_found`, `pairing_busy`, `rate_limited`, `already_paired`, `not_paired`,
`client_limit`, `resource_limit`, `invalid_state`, `state_unavailable`,
`clock_failure`, `deadline_exceeded`, and `request_ended`.

Tests perform the full transcript with independently OpenSSL-generated test
identities and a separate client-side implementation. A fixed OpenSSL AES vector
checks byte compatibility. They verify actual host/client signatures, bad PINs,
signature/certificate tampering, expiry, replay, owner and administrator boundaries,
bounded pending attempts, identity/client import, persistence acknowledgment,
failed commits, and cancellation. The state transport harness models persistence;
real filesystem/runtime integration and real Moonlight qualification remain
separate requirements. The fixture private keys are deliberately public test
material and must never be used for deployed identity.

References: [Moonlight Qt pairing client](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/backend/nvpairingmanager.cpp),
[RustCrypto X.509 builder](https://docs.rs/x509-cert/0.2.5/x509_cert/builder/),
and [RustCrypto RSA](https://docs.rs/rsa/0.9.10/rsa/).

## Native TLS identity

Pairing also provides `amaterasu.identity/1`, whose C table is defined by the
ABI-only `amaterasu-identity` crate. It serves pairing's effective host certificate
and committed client records. With `host_configuration` enabled, the configured
TLS files are authoritative; otherwise, pairing uses its persisted identity. The
private key remains inside pairing, and no key-export operation is provided. Add a
`native_provide` grant for this service to pairing.
Consumers need both its `native_use` grant and membership in pairing's existing
`transport_principals` setting. Administrator membership alone does not authorize
native binding. A consumer warming through the public `identity` control action
also needs the existing pairing/state delegated call grants.

Bind major 1/minor 0 with `size_of::<IdentityApi>()` through the unsafe native SDK.
The table has the common `NativeInterface` prefix and three functions:

| Function | Input | Successful output |
| --- | --- | --- |
| `certificate(context, ReplySink)` | None | One host certificate in DER |
| `sign(context, scheme, BorrowedBytes, ReplySink)` | Full TLS message | One RSA signature |
| `verify_client(context, BorrowedBytes)` | Client certificate DER | `STATUS_OK` for exact committed membership and valid time |

Inputs are bounded to 64 KiB per signing message and 8192 bytes per certificate.
Signature output is at most 512 bytes. Reply sinks borrow bytes only for the
synchronous callback; callers copy what they retain. Invalid input, unsupported
schemes and unavailable state never invoke the reply sink. No reply contains the
private key, its PEM encoding, RSA factors or client credential secrets.

Supported TLS SignatureScheme identifiers are RSA PKCS#1 v1.5 SHA256/384/512
(`0x0401`, `0x0501`, `0x0601`) and RSA-PSS RSAE SHA256/384/512 (`0x0804`, `0x0805`,
`0x0806`). The implementation hashes the full message exactly once, uses the existing
RustCrypto RSA implementation with randomized private-operation blinding, and uses
fresh digest-sized PSS salt. SHA1, ECDSA, RSA-PSS-PSS key schemes and arbitrary
caller-provided digest algorithms are unsupported. This native interface introduces
no new cryptographic dependency beyond the module's existing vetted primitives.

Statuses are generic ABI `STATUS_OK`/`STATUS_INVALID_ARGUMENT`/`STATUS_INTERNAL_ERROR`,
plus `IDENTITY_UNAVAILABLE=20`, `IDENTITY_FORBIDDEN=21`,
`IDENTITY_UNSUPPORTED=22`, and `IDENTITY_UNTRUSTED=23`. Certificate verification
checks the exact canonical DER of a currently valid paired client, not just a name
or public-key match. The transport must independently verify TLS CertificateVerify
proof of possession and enforce its protocol/version policy. Existing authenticated
connections are not forcibly disconnected by this library; transports must recheck
trust when required, and must not let session resumption bypass revocation.

### Committed snapshot and availability

When the platform enables `host_configuration`, the effective `pkey` and `cert`
paths provide the TLS identity used by pairing and its native identity API. If both
files are absent on first use, pairing generates a self-signed RSA identity and
creates the configured files without replacing existing paths. Missing parent
directories are created with Unix mode `0700`, and new Unix key and certificate
files are mode `0600`. If only one file exists, or either existing file is unsafe
or invalid, pairing fails closed and leaves the configured material untouched.
With the platform provider disabled, pairing continues to use its persisted
identity.

At startup, native identity is unavailable until an identity/read operation
successfully loads and validates durable pairing state (or initialization/import
successfully commits) and the configured TLS files validate. The public `identity`
control action is the normal warm/reload path. Ordinary read-only loads keep the
last known committed snapshot available; a successful same-revision reload
preserves it, avoiding spurious failures for in-flight TLS handshakes. A changed
revision replaces the entire snapshot atomically.

Before a potentially durable mutation, pairing suspends the native snapshot. The
new identity/client set is published only after the state service acknowledges its
successful revision commit. A write that fails or has an uncertain acknowledgement
never authorizes new clients; native calls remain unavailable until a subsequent
successful durable load establishes the committed state. Invalid/failed loads and
runtime transport failures also invalidate the snapshot. Revocation immediately
suspends native trust and its successful commit publishes a set without the revoked
certificate. Expired paired certificates remain listable/removable in control state
but are never accepted natively. Native operations validate host/client time on every
call and do not undo an observed expiry when the wall clock moves backward.

All native methods support concurrent calls while the binding remains live. Readers
pin immutable snapshots, perform RSA work outside the publication lock, then check
that the same snapshot remains current before returning success. Invalidation or
replacement during that work rejects its result. A call linearizes at its final
snapshot check; a concurrently subsequent revocation governs later operations. No
provider lock is held while calling a foreign ReplySink, so sink reentry does not
hold up snapshot publication. The module owns no native signing worker threads.

Signed dependencies must stop/join all transport calls and release their bindings
before pairing is destroyed or its DLL unloaded. Pairing destruction invalidates
remaining snapshots as defense in depth; retaining an Arc is not permission to call
unloaded code. Native grants express authority among trusted libraries, not a
native-code sandbox. Out-of-band privileged edits to the state backing store are
not an ordinary revocation path; use pairing operations to synchronize trust.

Tests verify every supported signature scheme against the host public certificate,
message tampering, randomized PSS, bounded input/output, wrong-principal binding,
exact client membership, revocation, expiry, same-revision refresh, commit-before-trust,
failed persistence/load invalidation, concurrent snapshot lifetime and callback lock
freedom. Real TLS transport interoperability belongs to the consuming transport's
integration tests; this module does not implement sockets or a TLS protocol stack.

The sibling `nvhttp` library now implements the Gen7 HTTP/XML adapter and delegates
TLS signing to this native identity API. `scripts/test_nvhttp.py` verifies the
complete transcript, TLS 1.2/1.3, exact host certificate identity, persisted pairing
and revocation through real loaded libraries. An actual Moonlight application and
end-to-end streaming remain unqualified.

## Legacy host migration

The public `migration` API accepts only the current default
Sunshine/Vibeshine pairing layout: one state file containing
`root.uniqueid` and `root.named_devices`, plus
`credentials/cakey.pem` and `credentials/cacert.pem`. A configured `pkey` or
`cert` is accepted only when its canonical regular file remains beneath the
supplied predecessor root; absolute paths, traversal, links, and conflicts are
rejected. It reads a supplied predecessor root without following links, checks the
self-signed RSA host certificate and its PKCS#8 private-key ownership, and
validates each enabled paired-client certificate before returning an opaque
import object. Malformed, conflicting, old `devices`-format, alternate-key,
or over-capacity sources fail closed.

`seed_pairing_state` holds the normal state-root lock and writes the pairing
module's ordinary caller namespace in the same versioned format as the state
module. It writes only an empty namespace and leaves any existing Amaterasu
pairing state untouched. Its errors are fixed codes; neither API exposes
legacy key, certificate, or credential contents.

The installer process boundary is the sibling executable
`amaterasu-pairing-import.exe`: `validate --source SOURCE` performs no writes,
and `seed --source DATA_ROOT/legacy-imports/PRODUCT --state STATE_ROOT` commits
only validated data. Its output contains only fixed status words.

Sparse software encoder overrides support exact `sw_preset`/`sw_tune` strings
(up to 128/256 UTF-8 bytes, no controls), `encoder: "auto" | "software"`, and
a requested `min_threads` integer from 1 through 256 (default 2). Null/absence
inherit; empty tuning strings remain explicit. The actual x264 backend validates
codec vocabulary when opening software and may cap the effective thread count
based on frame size or runtime limits. NVHTTP resolves each field client before
application before global and freezes the launch snapshot.
See [software policy](../nvhttp/README.md#software-encoder-policy) for runtime
availability, source differences and remaining qualification gates.
