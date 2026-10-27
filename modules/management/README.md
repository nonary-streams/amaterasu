# Management module

`amaterasu-management` is a native cdylib/rlib providing `amaterasu.management/1`.
The generic host does not implement HTTP or product routing. This module uses
`httparse` for HTTP framing, the Rust standard library's nonblocking TCP sockets,
rustls for optional browser HTTPS, and the module SDK/serde/serde_json. No
networking thread outlives the library.

The listener is disabled by default. Development bootstrap settings for principal
`amaterasu.management` can explicitly enable it:

```json
{"enabled":true,"bind":"127.0.0.1:0","allowed_origins":["http://127.0.0.1:3000"]}
```

`bind` must be an explicit IPv4/IPv6 loopback socket address. Port zero chooses an
ephemeral port. The trusted console can request
`amaterasu.management/1` with `{"action":"status"}` to obtain
`{"ok":true,"enabled":true,"bound_address":"127.0.0.1:PORT"}`. Disabled status
returns a null address. Startup rejects wildcard/nonloopback plaintext binds and invalid
settings. At most 16 exact HTTP(S) origins without paths are accepted; the bound
transport's exact origin is always permitted. `Host` must exactly match the bound numeric
address and port, including IPv6 brackets. DNS names such as `localhost` are
intentionally rejected to prevent DNS rebinding.

An opt-in browser HTTPS listener uses three signed bootstrap settings together:
`tls_bind` (explicit socket address), `tls_certificate_path` (absolute PEM chain),
and `tls_private_key_path` (absolute PEM key). The TLS files are bounded at load
time and rustls checks that the key matches the leaf certificate. The trusted
console status includes `tls_address` when bound. A deployment must provision a
separate browser-valid leaf with the selected listener IP SANs, serverAuth EKU, a browser-trusted
issuer, and private-key file access restricted to the host account and admins.
Replace certificate and key atomically and restart management to rotate them.
The Moonlight pairing identity and its certificate are not browser TLS material.
The installed host additionally requires nonzero TLS ports and content-addressed
files under its private `management-tls` state directory, selected through the
worker-state-root token. File digest, ownership, permissions, containment and
reparse checks remain mandatory. The development Linux setup tool generates or validates a separate identity for
the selected assigned private address; selecting a remote bind alone does not
qualify a certificate. See [setup and enrollment](../../docs/linux-session-profile.md#browser-https-on-a-private-network).

`origin_web_ui_allowed` selects the maximum source tier for HTTPS ingress:
`pc` permits IPv4 127/8 and IPv6 ::1, `lan` additionally permits RFC1918,
100.64/10, 169.254/16, fc00::/7 and exactly fe80::/64, and `wan` permits the
remaining addresses. IPv4-mapped IPv6 peers are classified as IPv4. Plain HTTP
and `/healthz` remain PC-only. This policy uses the actual socket peer; forwarded
headers, browser Origin, cookies and credentials cannot change its source tier.

The Settings page reads `/v1/management_settings/get` and writes
`/v1/management_settings/update` with `expected_revision` and an optional
`origin_web_ui_allowed` and `csrf_allowed_origins`. Omission retains each selection;
null resets the peer maximum to the source default `lan` and CSRF exceptions to
the empty default. The management module owns a separate version-2 product
document in configuration's caller-scoped namespace, leaving its generic user
schema intact. Writes require the immutable management runtime principal after
HTTP scope verification. Confirmed CAS values apply to new and existing
connections and survive restart. Ingress stays closed until the durable document
loads, and during ambiguous writes or unresolved child-result cleanup. Narrowing
access can disconnect a browser whose peer no longer qualifies.

`csrf_allowed_origins` is an explicitly selected list of at most 16 distinct
HTTPS origins, each no longer than 256 bytes. Origins contain an ASCII DNS name,
IPv4 address, or bracketed IPv6 address and an optional nonzero port; paths,
credentials, wildcards, fragments, queries, and ambiguous numeric host spellings
are rejected. Exact strings are preserved and matched against `Origin`, including
the port. Use the origin serialized by the browser. Null selects the empty default;
an explicit `[]` remains distinct in durable readback. Version-1 documents migrate
to no exceptions. No origin is implicitly exempted, including loopback.

On HTTPS, a listed nonmissing `Origin` may skip the cookie CSRF origin/header
predicate. Unlisted requests retain exact same-origin plus
`X-Amaterasu-CSRF: 1` checks. An exception grants no authentication: the host-only
Secure/HttpOnly/SameSite=Strict cookie still contains a verified access or refresh
credential, and protected routes still verify the exact scope. Host, socket-peer
admission, and the separate startup `allowed_origins` browser policy remain
mandatory. Listing a different origin does not add it to CORS or enable
credentialed cross-origin browser access. Browser-auth preflight remains denied.
Removing an exception closes its active dependent connections on the next tick;
effects already completed before removal cannot be undone.

This is a bounded subset of pinned Vibeshine's CSRF policy. Its built-in loopback
exceptions, prefix/port/path matches, Referer and missing-Origin exceptions,
minted `X-CSRF-Token` tokens, and `csrf_token` query fallback are not reproduced.
`global:csrf_allowed_origins` remains pending complete parity and installed-host
browser/platform qualification.

## Authentication and routes

A trusted local console can initialize authentication or generate a short-lived
one-time first-administrator enrollment code. The browser can exchange that code
at `/v1/auth/enroll`; enrollment fails after initialization. The development
Linux enrollment helper issues the code under the stopped host's root lock.
Account administration forwards the session bearer to authentication, which
checks the administrator role itself; changing one's own password requires the
current password. There is no arbitrary runtime command or direct state API.
Keep management out of authentication `administrator_principals`; its narrowly
scoped `legacy_verifier_principals` permission supports legacy request checks.
Pairing has its own separate administrator principal policy.

All business routes are POST and accept `Content-Type: application/json` with an
object body. The route supplies `action`; including `action` in the body is an
error. Remaining body fields are the target module's documented action arguments.

`POST /v1/auth/login` accepts `{"username":"...","password":"..."}` with an
optional boolean `remember_me` and delegates to `amaterasu.authentication/1`.
`POST /v1/auth/refresh` accepts a `refresh_token` body field and rotates the access
and refresh pair. `POST /v1/auth/logout` accepts exactly one of `bearer` or
`refresh_token` and revokes the pair. These three endpoints require no access
header because login and refresh establish the credential; the refresh secret is
validated by the authentication module. Successful login returns the auth
library's JSON, including both token secrets. Every other protected route requires
`Authorization: Bearer ama_<opaque token>` or, over HTTPS only, the access cookie
with same-origin CSRF checks. Query parameters and body fields do not provide
authentication. Before each business request, the module
calls auth `verify` with `bearer` and `required_scopes:[scope]`, requires an explicit
successful result containing the exact scope, then dispatches the action.

The legacy token endpoints above remain available for API clients. On HTTPS,
`/v1/auth/browser/login` accepts the login body, while `/browser/refresh`,
`/browser/logout`, and `/browser/session` accept `{}` and use HttpOnly cookies.
Successful browser login and refresh return session metadata without token secrets,
and set host-only Secure, HttpOnly, SameSite=Strict access and refresh cookies scoped
to `/v1` and `/v1/auth/browser` respectively. Remembered refresh cookies persist
until the signed authentication expiry. Browser session verifies the access cookie;
a remembered client can refresh after a browser restart when that session cookie
is absent. Logout revokes the refresh pair (or the access session if refresh is
missing) and clears both cookies. With neither valid cookie, it clears them
locally and returns `session_missing` without claiming server revocation. A failed
refresh never clears cookies because another tab may have completed rotation.
Cookie-authenticated POSTs require an exact same HTTPS Origin and
`X-Amaterasu-CSRF: 1`; browser routes do not send credentialed CORS headers.
Plaintext requests cannot use browser cookie routes or cookie authentication.

| Route prefix | Actions | Required scope |
| --- | --- | --- |
| `/v1/accounts/` | list, create, update, delete, set_password | `configuration.write` and administrator session |
| `/v1/auth/` | change_password | `account.self` and current password |
| `/v1/auth/` | settings, settings_update | `configuration.read`, `configuration.write`; administrator session |
| `/v1/extensions/` | list, call | `account.self` plus each approved method scope and role |
| `/v1/catalog/` | get, list, search | `catalog.read` |
| `/v1/catalog/` | create, update, delete | `catalog.write` |
| `/v1/configuration/` | get | `configuration.read` |
| `/v1/configuration/` | register, update, migrate | `configuration.write` |
| `/v1/management_settings/` | get | `configuration.read` |
| `/v1/management_settings/` | update | `configuration.write` |
| `/v1/host-configuration/` | get | `configuration.read` |
| `/v1/host-configuration/` | update | `configuration.write` |
| `/v1/session/` | list, snapshot | `session.read` |
| `/v1/session/` | create, activate, stop, finish_stop, cancel, fail, forget, acquire, release | `session.write` |
| `/v1/application/` | list, status | `application.read` |
| `/v1/application/` | launch | `application.launch` |
| `/v1/application/` | terminate | `application.terminate` |
| `/v1/steam/` | list, status | `steam.read` |
| `/v1/steam/` | refresh, force_sync | `steam.refresh` |
| `/v1/history/` | settings_get | `configuration.read` |
| `/v1/history/` | settings_update | `configuration.write` |
| `/v1/history/` | list_sessions, session_detail, active_sessions, history_status | `session.read` |
| `/v1/history/` | delete_session | `session.write` |
| `/v1/lutris/` | artwork_read | `lutris.read` |
| `/v1/diagnostics/` | query, counters, status | `diagnostics.read` |
| `/v1/diagnostics/` | record, increment | `diagnostics.write` |
| `/v1/pairing/` | identity, pending, list_clients, approve_pin, unpair | `pairing.admin` |
| `/v1/display/` | status, enumerate | `display.read` |
| `/v1/display/` | apply, restore, recover | `display.write` |
| `/v1/capture/` | select_portal_source, cancel_portal_selection | `capture.write` |
| `/v1/capture/` | poll_portal_selection | `capture.read` |
| `/v1/launch/` | start | `application.launch` |
| `/v1/launch/` | status, list | `application.read` |
| `/v1/launch/` | stop | `application.terminate` |

On Linux, an authenticated operator selects a desktop source before launching
Desktop: `POST /v1/capture/select_portal_source` with
`{"source_type":"monitor","operation_id":"desktop-1"}`, then polls
`/v1/capture/poll_portal_selection` with the same operation ID until ready.
The current-user portal displays the consent prompt. The response never exposes
the PipeWire node handle; the signed video component retains the selected
source for the next Desktop stream. The operator can cancel with
`/v1/capture/cancel_portal_selection`. These routes do not open a capture
stream by themselves.

Host-configuration `update` forwards the provider's complete version-1 document:
`expected_revision`, `file_state`, `vibeshine_file_state`, `credentials_file`,
`pkey`, `cert`, `log_path`, and `min_log_level`. The management route requires
`configuration.write`; the provider also restricts updates to the management
principal. The logging editor changes only `log_path` and `min_log_level`, sends
the other fields verbatim from the signed `get` response, and confirms the new
revision and values through another signed read. A caller with
`configuration.write` can intentionally change the other host-configuration
paths through this API. The editor separately reads signed diagnostics `status`
to show the revision actually adopted by the logging worker. A saved revision
can remain pending if the worker has not refreshed or cannot open its file sink.

Example authenticated request body for `/v1/catalog/create`:

```json
{"expected_revision":0,"entry":{"name":"Example","launch":{"kind":"application","path":"/Applications/Example.app"}}}
```

`POST /v1/steam/force_sync` accepts only `{}` and requires `steam.refresh`. It
dispatches the fixed `force_sync` action to `amaterasu.steam/1` as
`amaterasu.management`. This is an in-memory, one-shot request; it does not add
a persisted force-sync setting or require a Steam schema version change. It is
available even when automatic catalog sync is disabled.

`POST /v1/lutris/artwork_read` accepts exactly `game_id` (positive signed 64-bit
integer), `revision` (64 lowercase SHA-256 hex digits), and `offset` (an 8 KiB
multiple below 16 MiB). It forwards a bounded artwork chunk to
`amaterasu.lutris/1` after verifying `lutris.read`.

Business service JSON is returned unchanged with HTTP 200, including domain errors
such as revision conflicts; clients must inspect `ok`. Missing bearer yields 401,
failed verification or insufficient scope 403, login failures 401, auth rate limits
429, framing/route errors 4xx, and unavailable runtime services 503. Oversized
service responses yield 502. No credentials, request bodies, or tokens are logged.
Successful login intentionally returns a secret; responses use `Cache-Control:
no-store`. Clients must handle credentials securely.

All HTTP clients share the `amaterasu.management` runtime principal and its product
data namespaces. Token subjects are not fabricated runtime identities, and this
API provides no per-user tenant isolation. Runtime calls originate at network
ingress with no parent; packaged management grants constrain available services,
and business modules propagate their own child request deadlines/authority.
Required grants are provide `amaterasu.management/1` and call authentication,
catalog, configuration, session, application, steam, diagnostics, pairing, display,
launch, and state service
version 1. The state grant supports those modules' delegated persistence calls.
Native modules are trusted code; grants are not a native-code sandbox.

## Pairing administration, display control, and coordinated launch

These are explicit authenticated administrative routes, not a Moonlight wire
transport. All reject unknown top-level fields before service dispatch. Display
apply also rejects unknown fields inside configuration, display, and refresh
objects. The native modules retain semantic validation, ownership checks and
platform-effect policy. No private key or raw client-certificate import is
available over management HTTP.

Pairing routes require **`pairing.admin`** even for public identity and metadata:

| POST route | JSON body |
| --- | --- |
| `/v1/pairing/identity` | `{}` |
| `/v1/pairing/pending` | `{}` |
| `/v1/pairing/list_clients` | `{}` |
| `/v1/pairing/approve_pin` | `{"pairing_id":"<pending id>","pin":"0123"}` |
| `/v1/pairing/unpair` | `{"certificate_id":"<64 hex fingerprint>"}` |

The PIN must be a four-digit string, preserving leading zeros. Initialize/import
of host identity and direct client trust imports remain trusted-console-only.
The Gen7 begin/challenge/secret phases and certificate authentication checks stay
with the transport module; they are not HTTP management routes. The owner-only
cancel operation is also not exposed because management does not own transport
transcripts. An administrator approves a pending transport request by its opaque
pairing ID. Identity returns only public certificate metadata; PINs and keys are
never included in management error messages or logs.

For these routes, configure pairing `administrator_principals` to include
`amaterasu.management` alongside the trusted console. Do **not** add management to
pairing `transport_principals` or authentication `administrator_principals`.
Management's explicit route allowlist is the browser-to-administrator boundary.

Display routes:

```text
POST /v1/display/status     {}
POST /v1/display/enumerate  {"offset":0,"limit":16}
POST /v1/display/enumerate  {"display":"<monitor id>","offset":0,"limit":32}
POST /v1/display/apply      {"configuration":{"displays":[{"id":"<monitor id>","width":1920,"height":1080,"refresh":{"numerator":60000,"denominator":1001},"x":0,"y":0,"rotation":"identity"}]}}
POST /v1/display/restore    {"lease":"<returned lease>"}
POST /v1/display/recover    {"lease":"<returned lease>"}
```

Enumeration accepts optional/null display, offset and limit; limits are 1–32.
Apply specifies the **entire desired active display set**, so omitted displays
are disabled. A write-scoped token does not enable platform mutation: display
settings still default `allow_mutation:false`. Restore/recover require the exact
lease owned by the management runtime principal; payload ownership overrides are
rejected. An apply acknowledgement may be lost after physical effects, so consult
status and the durable recovery journal before retrying. macOS/Linux currently
report unsupported native enumeration/mutation. No management test changes the
developer's display topology.

Coordinated launch routes use the existing application scopes:

```text
POST /v1/launch/start   {"operation_id":"browser.launch.1","app_id":"app-0000000000000001"}
POST /v1/launch/status  {"launch_id":1}
POST /v1/launch/list    {"offset":0,"limit":16}
POST /v1/launch/stop    {"launch_id":1}
```

`start` maps explicitly to service action `launch`. It accepts only a printable
operation ID and a stable catalog application ID; it cannot supply an executable,
arguments, environment, caller, process ID, or session ID. The launch library
resolves catalog metadata using the initiating management namespace and owns
process/session coordination. Status/list/stop use its launch ID. An active launch
means application/session coordination, **not stream readiness**. Initialization,
cleanup, idempotency and platform support remain the launch module's contract.

Profile additions include scopes `pairing.admin`, `display.read`, `display.write`,
management call grants and package dependencies
for pairing/display/launch, and the pairing administrator setting above. The
launch principal must be permitted by catalog's launch-resolution policy. No new
cryptographic or platform dependency is introduced into management routing.

## HTTP bounds and browser protections

The server accepts at most four new connections per tick, retains at most 16
active connections, and performs at most one 4 KiB read or 16 KiB write per connection
per tick. Each connection has a ten-second lifetime covering input, authentication,
service dispatch and response. Pending runtime work is cancelled at expiry and
the connection closes. Deadlines do not undo already committed mutations; clients
must reconcile ambiguous outcomes before retrying. No mutations are replayed. Internal requests use unique operation IDs and release
completed runtime tracking immediately; retries cannot recover those internal
results after delivery.

HTTP/1.1 only: at most 32 unique headers and 8192 header bytes; JSON body at most
12,000 bytes; JSON response at most 24,000 bytes plus fixed headers. Requests need
one decimal Content-Length. Duplicate headers, Transfer-Encoding, Expect, Upgrade,
unsupported methods, bodies exceeding their declared length and already-buffered
pipelining are rejected. Only one request is handled per connection, which always
closes. Keep-alive, chunking, compression, streaming and multipart data are absent.

Origin-bearing requests must match the configured allowlist exactly; `null`,
wildcards and unapproved origins are denied. Cross-site Fetch Metadata without an
Origin is denied. Only explicit OPTIONS preflight for a known route, POST and the
`authorization`/`content-type` headers is accepted. Allowed origins receive exact
CORS headers, never a wildcard or credential-cookie permission. Strict JSON,
explicit Bearer headers, Origin checks and numeric Host validation protect the
browser boundary; ordinary nonbrowser clients may omit Origin. HTTPS browser
cookie routes retain their separate same-origin CSRF checks.

## Signed web extensions

An extension is a separately signed bundle, locally approved by its explicit ID,
publisher key and exact API method declarations. The selected signed manifest
must match the approved ID; broker-provided API metadata and the session's
role and scope constrain each call. Publisher declarations alone do not grant
authority. See the [example and policy](../../examples/web-extension/README.md).

Extension JavaScript runs with the web UI's same-origin authority. It is trusted
code, not a sandbox, and the method allowlist cannot contain all consequences
of a malicious script. To withdraw an extension from a development Linux root,
stop the host and use the [local withdrawal command](../../docs/linux-session-profile.md#browser-https-on-a-private-network).
It removes the approval and advances the authentication generation; close all
previously open tabs because already-loaded code can continue running. This is
local withdrawal, not publisher-wide key revocation.

## Remaining release gates

Plain HTTP remains a loopback development surface. Physical remote HTTPS
reachability, installed certificate/service provisioning, supported-platform behavior, legacy
Sunshine/Vibeshine HTTP compatibility, real-client pairing and production
load/abuse qualification remain release gates. Scoped launch authority
can execute trusted native applications and is intentionally powerful.

Run `cargo test -p amaterasu-management` and `python3 scripts/test_management_api.py`.
The latter starts a real signed package under `amaterasu serve`, binds an ephemeral
loopback port, performs actual auth and catalog CRUD, and checks more than 512 internal calls plus scoped token
persistence/revocation, session invalidation, browser protections and framing.
A sandbox must permit loopback listening for that integration test.
`python3 scripts/test_management_origin_policy.py` exercises a real signed host
and browser TLS identity on loopback: scoped policy CAS, null/omitted updates,
Host/Origin checks, generic schema isolation and restart persistence. Pure unit
tests cover every remote CIDR boundary without opening remote listeners.

The Linux gate `signed_private_network_peer_admission_and_narrowing` is ignored
by default because it opens non-loopback test listeners. Its launcher requires
a normal user, creates disposable user/network namespaces, and verifies that
only the private loopback interface exists before assigning test addresses.
It changes no host interface, route or firewall rule. With built workspace
artifacts, Python `cryptography`, `unshare` and `iproute2` available, run:

```sh
cargo build --workspace --locked
AMATERASU_PYTHON=/path/to/python-with-cryptography cargo test -p amaterasu-management \
  signed_private_network_peer_admission_and_narrowing -- --ignored
```

The signed host uses a fresh trusted certificate with the actual listener IP
SAN. The gate rejects untrusted/mismatched TLS identity, exercises actual
PC/LAN/WAN socket peers, checks authorization and PC-only health, disconnects
previously admitted peers on narrowing, and checks restart persistence. This
qualifies the Linux admission path in a controlled private network; physical
LAN/WAN reachability and installed TLS/service provisioning remain release gates.

Verified static frontend bundles are optional; see [web bundles](../../docs/web-bundles.md).
GET assets bypass API authentication and expose only signed public frontend bytes.
API POST requests from the exact bound HTTP origin are automatically permitted;
other browser origins require the configured allowlist. Bearer/scope checks still
apply. Connection reads remain 4 KiB per tick; response writes are now 16 KiB.

Media discovery uses `POST /v1/media/dxgi_outputs`, `wgc_outputs`, `x11_outputs`,
`kms_outputs`, `audio_endpoints`, `audio_driver_status`, `encoders`, `audio_encoders`,
and `video_status`, each with an empty JSON object and the `media.read` scope. These
routes pin their provider/action and reject additional fields. They
expose no native pointers, arbitrary service forwarding, or capture/open controls.
Device enumeration can report an unsupported platform; default-disabled audio and
encoding return empty capability lists without starting capture or encoding.
