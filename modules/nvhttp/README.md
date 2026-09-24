# Moonlight HTTP/TLS adapter

`amaterasu-nvhttp` is an independently built Rust module. It supplies the
Moonlight Gen7 pairing HTTP/XML adapter and the bounded transport described below;
cryptography and durable client trust remain owned by `amaterasu.pairing/1` and
`amaterasu.identity/1`. Native modules are trusted code, not sandboxed by grants.

Startup defaults are disabled, the machine hostname, HTTP `127.0.0.1:47989`,
HTTPS `127.0.0.1:47984`, and `allow_remote:false`. If the platform hostname API
fails or returns a value outside NVHTTP's bounds, startup uses `Amaterasu` as a
last-resort name. A blank configured hostname means use the machine hostname;
nonempty names are preserved without trimming or case normalization. The transport
owns listener settings and reports the actual bound ports. Public routes do not
initialize a private host identity: an administrator must initialize/import one
through pairing first. The configured hostname is the bootstrap fallback. After
startup, NVHTTP loads `product/amaterasu.nvhttp` from
`amaterasu.configuration/1`; the committed effective hostname replaces the fallback
in subsequent server-info replies. NVHTTP needs configuration calls in addition to
pairing/state calls for delegated transcript operations and native use of the
identity service for TLS signing and live client checks.
Catalog export additionally requires a catalog call grant and an explicit catalog
`export_principals` mapping from `amaterasu.nvhttp` to the desired owner (the
platform profile uses `amaterasu.management`). The HTTP caller cannot select the
owner. It does not need administrative PIN approval or unpair authority.

## Product hostname settings

The management API exposes `GET /v1/nvhttp/settings` and
`POST /v1/nvhttp/settings_update`. Only requests authenticated and forwarded by
`amaterasu.management` are accepted by NVHTTP. The frontend requires the
`configuration.read` scope for reads and `configuration.write` for updates.

Reads return both the effective value used by Moonlight and the configured override.
An empty `configured_hostname` means the machine hostname is active; `hostname`
remains the effective name for compatibility:

```json
{"ok":true,"hostname":"Studio PC","configured_hostname":"","port":47989,"https_port":47984,"active_http_port":47989,"active_https_port":47984,"listener_error":null,"revision":3}
```

An update sends a whole hostname replacement and its observed revision. Sending an
empty name stores the default selection and uses the machine hostname:

```json
{"hostname":"Studio PC","port":47989,"expected_revision":3}
```

The successful update response has the same shape as a read and is returned only
after the configuration service confirms the persisted write. The `hostname` and
`configured_hostname` fields distinguish the effective Moonlight name from the
stored override. Names are limited to 128 UTF-8 bytes and reject control
characters. A stale revision returns `revision_conflict` with `current_revision`.
When a write result is
ambiguous, NVHTTP reads the product document again and returns
`settings_outcome_unknown`; it never reports success based only on the requested
hostname. The readback still updates server-info to the hostname currently stored.
While configuration is unavailable, the bootstrap hostname remains active,
settings reads and writes fail with a configuration error, and NVHTTP retries the
load with backoff. A missing product document uses the bootstrap hostname and
configured component port at revision 0; its first successful update creates
revision 1.

## Moonlight listener base port

The NVHTTP product settings editor also stores a Moonlight HTTP base port. It
accepts 1029 through 65514; the HTTP listener binds to that port and HTTPS to the
base port minus five. This matches Vibeshine's NVHTTP port offsets and keeps both
ports in range. The same setting configures RTSP TCP at base+21 and the per-stream
UDP source ports at base+9 (video), base+10 (control), and base+11 (audio). The
RTSP listener retains its configured bind IP; the NVHTTP control listener remains
loopback unless startup policy explicitly allows remote binding.

Updating the base port prepares both NVHTTP replacement listeners before releasing
either current listener, retains accepted connections, and updates `/serverinfo`
to advertise the new bound ports. RTSP changes wait until all live RTSP tickets
are terminal and accepted RTSP TCP connections have closed. While that change is
pending, new RTSP opens keep using the active base port. The RTSP module checks that
all three UDP addresses can bind together before acknowledging application, then
passes those addresses to `stream-session` for each later stream. Per-session UDP
sockets are created by the stream owner and are not reserved while idle, so an
external process can still claim a port after preflight.

A successful update is acknowledged only after both NVHTTP listeners and the RTSP
listener/UDP bind preflight are active at the requested base. If an RTSP stream is
active, a TCP or UDP target is occupied, or a listener cannot bind after
persistence, NVHTTP returns `listener_apply_pending` with `persisted:true`, retains
the previous active ports, and retries applying the stored port. Restart loads the
stored base and repeats reconciliation. The settings response includes
`active_rtsp_port` and `rtsp_listener_error` alongside the active HTTP ports.

The live settings response includes the configured port, derived https_port, and
active_http_port/active_https_port values. A legacy version 1 hostname-only product
document remains readable and uses the component's configured HTTP port; the next
update writes a version 2 document containing both hostname and port. This NVHTTP
slice keeps its configured bind IPs unchanged and loopback restrictions intact. It
does not yet apply the global `address_family` or `bind_address` settings.

Public streaming is a separate startup opt-in. Omitting `streaming` disables it.
The complete settings object has no executable, arguments, environment, or catalog
owner field:

```json
{
  "streaming": {
    "capture": {
      "capture_provider": "dxgi",
      "output": "display-one",
      "encoder": 0,
      "audio_endpoint": "default"
    },
    "profile": {
      "max_width": 1920,
      "max_height": 1080,
      "max_fps": 60,
      "bitrate_kbps": 10000,
      "max_reference_frames": 1,
      "packet_size": 1024,
      "fec_percentage": 20,
      "min_parity_shards": 2,
      "max_fec_blocks": 1,
      "packet_ms": 10,
      "audio_bitrate": 128000,
      "encrypt_video": true,
      "encrypt_audio": true,
      "permit_legacy_audio_iv_wrap": false
    }
  }
}
```

Unknown or missing nested fields fail startup. Maximum dimensions must be even and
within 16–3840 by 16–2160; maximum FPS is 1–120. Bitrate, audio, packet duration,
and reference-frame bounds are validated before listeners start. Video FEC and
packetization settings are additionally checked by the same packetizer used for
the stream.

## Wire routes

All routes accept a bodyless GET. Query parsing allows at most 12,288 request-target
bytes and 24 fields. Duplicate fields are rejected after percent-decoding, so
`uniqueid` and `unique%69d` cannot supply competing identities. Invalid UTF-8,
malformed escapes, controls, missing `=`, and duplicate phase selectors fail.
`+` decodes as space. `uuid`, `updateState`, and unrelated bounded client metadata
are not authority. XML values escape all markup delimiters and replace XML-invalid
BMP noncharacters U+FFFE/U+FFFF and controls.

- `/serverinfo` retrieves the persistent host UUID through pairing. Plain HTTP
  always returns `PairStatus=0`; HTTPS requires a currently paired client TLS
  certificate, returning `PairStatus=1` only after the transport's live check.
  An untrusted HTTPS request returns 401, allowing Moonlight to fall back to HTTP.
- `/pair` implements the Gen7 phases below, including the held initial request.
- `/applist` requires trusted paired TLS and exports the configured catalog owner
  as Moonlight `App` elements with `AppTitle`, numeric `ID`, and
  `IsHdrSupported=0`. An empty successful list means that catalog is actually empty.
- `/launch` requires trusted paired TLS and the validated launch query described
  below. It returns success only after the exported application is active and the
  encrypted RTSP ticket is ready.
- `/cancel` requires trusted paired TLS and scopes cancellation to the exact paired
  certificate that owns the active launch. Query `uniqueid` is metadata, not
  authority. A foreign paired certificate cannot inspect or stop the owner record.
- `/resume` and `/unpair` require trusted paired TLS and return 501. Resume is not
  reported as implemented by replaying a fresh launch.

Without streaming configuration, server info advertises `appversion=7.1.0.0` and
`StreamingSupported=0`. Configured streaming advertises `appversion=7.1.431.0` and
`StreamingSupported=1`. This reports accepted startup policy; it is not a runtime
GPU, encoder, or capture probe. `ServerCodecModeSupport` and unavailable hardware
fields remain zero rather than claiming codecs, HDR, a public IP, or a hardware
MAC. `state` is globally busy while any owned launch remains active or has
unconfirmed cleanup. `currentgame` contains the app ID only for the owning paired
certificate and remains zero for other clients.

Responses use XML `<root status_code="..." status_message="...">`; the HTTP
status matches the root status. Successful cryptographic phases return `paired=1`
only after the pairing service has acknowledged that phase. Runtime/service error
text does not expose private keys, PINs, or internal cryptographic details.

## Catalog export

The adapter reads `export_list` in pages of 32 and requires one unchanged catalog
revision throughout the request. It validates each page and strictly ascending
IDs, including the continuation cursor. Canonical `app-` plus 16 lowercase hex
digits maps directly to its integer in `1..=2147483647`; out-of-range IDs fail
visibly rather than being hashed, truncated, or reassigned. Renames preserve IDs.
Titles are XML escaped. HDR support is not claimed.

There are at most four active lists, 16,384 entries, 1,024 pages, 30 seconds total,
and 1 MiB minus 1 KiB of XML per list. Byte, page, or time bounds may be reached
before the entry bound. Overflow returns 413, revision changes 409, malformed
provider data 502, provider failures 503, and expiry 504. No partial list is sent.
Catalog work is never dispatched for an untrusted HTTP/TLS request. Before a
completed list enters the transport write queue, the exact handshake-proven
certificate is checked against live paired trust again; revocation replaces the
private reply with 401.

## Public launch lifecycle

Moonlight launch accepts required `appid`, `mode=WIDTHxHEIGHTxFPS`, `rikey`, and
`rikeyid`. The application ID is a decimal catalog ID in `1..=2147483647`.
Dimensions are even and at least 16, and cannot exceed the configured maxima; FPS
is positive and cannot exceed `max_fps`. The selected values configure capture and
encoding for this ticket, but do not claim that the host changed its display mode.
`rikey` is exactly 16 hexadecimal bytes. `rikeyid` accepts Moonlight Qt's signed
32-bit decimal representation and the equivalent nonnegative unsigned 32-bit
representation, preserving the bit pattern.

The bounded normal metadata allowlist is `uniqueid`, `uuid`, `additionalStates`,
`sops`, the `clientHdrCap*` fields, `hdrMode`, `localAudioPlayMode`,
`surroundAudioInfo`, `remoteControllersBitmap`, `gcmap`, `gcpersist`, and
`corever`. Unknown fields are rejected, including executable, path, arguments,
environment, or catalog-owner overrides. The current profile accepts SDR,
stereo remote playback, core version 1, and `sops=0`. HDR, surround audio, local
audio playback, display optimization, and controller forwarding requests fail
before the application service is called. `additionalStates` is accepted only as
a decimal Boolean. These gates prevent successful XML from implying unsupported
media or input behavior.

The lifecycle calls the launch service's exported-app API with only the immutable
numeric catalog ID and a private operation ID. Catalog policy resolves the trusted
owner and executable. Once the service confirms the application is active, the
lifecycle opens a fresh encrypted RTSP ticket for the TLS peer IP using UDP peer
discovery and the configured capture/profile policy. The returned XML contains
`gamesession=1` and its `rtspenc://` URL only after both operations are confirmed.
Fresh launch records use unpredictable private operation identifiers and nonzero
RTSP connection data. Key material and discovery tokens do not enter public status
or logs.

The exact client certificate owns the application and RTSP records. Trust is
checked before admission, again before any private response enters the transport,
and on subsequent module ticks. Revocation, early disconnect, timeout, provider
failure, or a lost successful response starts owned RTSP and application cleanup.
After a successful launch, the lifecycle periodically checks both providers and
cleans up if either ends. A cleanup failure stays owned and keeps admission busy
instead of freeing a possibly live process or stream. A matching retry can recover
the same ready ticket without launching another application.

The server adopts a launch only after the complete HTTP response and TLS
`close_notify` have been written to the socket. This is a transport flush boundary,
not proof that the Moonlight process received or parsed the XML. A socket failure
before that boundary causes cleanup; a normal connection close after it does not
stop the adopted stream. `/cancel` performs the same owned cleanup and returns
`gamesession=0` only when it is confirmed.

## Pairing transcript

Each request includes the same `uniqueid` (1..64 identifier bytes). The adapter
accepts Moonlight's actual initial query:

```
/pair?uniqueid=CLIENT&phrase=getservercert&salt=HEX16&clientcert=HEX_PEM&devicename=NAME
```

`clientcert` is hex-encoded certificate PEM, at most 4096 decoded bytes; the
pairing authority validates the certificate itself. `salt` is exactly 16 hex
bytes. The adapter starts a private pairing-service transcript and retains its
opaque ID. Only an administrator approves the PIN through the existing pairing
control service. The original HTTP request remains open while the adapter polls
`get_server_cert` every 250 ms. Internal `pending:true,paired:0` replies are never
sent to Moonlight. Approval returns `paired=1` and hex PEM `plaincert`.

Subsequent separate HTTP requests contain exactly one phase field:

1. `clientchallenge=HEX16` → `challengeresponse` (48 encrypted bytes).
2. `serverchallengeresp=HEX32` → `pairingsecret` (host secret and RSA signature).
3. `clientpairingsecret=HEX_SECRET_AND_SIGNATURE` → `paired=1` only after the
   pairing authority reports durable certificate persistence.
4. HTTPS `phrase=pairchallenge` → `paired=1` only with TLS private-key possession,
   a live paired-certificate check, and the exact original transcript certificate.

No cryptographic phase is automatically retried. One phase may be in flight per
transcript, and out-of-order/replayed phases are rejected before calling the
pairing service. Public routes cannot approve a PIN or fabricate phase completion.

The private map binds the client ID to its original remote **IP**, initial DER
certificate, expected phase, opaque authority ID, and deadline. Source ports are
not used because Moonlight opens different connections for successive phases.
A supplied later `clientcert` or TLS peer certificate must match the original.
Replacing an existing client's certificate or source address cannot cancel its
transcript; duplicate begin and foreign phases fail without forwarding a cancel.

Moonlight omits certificates on later plaintext phases. Therefore remote-IP plus
`uniqueid` association is **not cryptographic peer authentication**. A party sharing
that IP who knows the ID may interfere with plaintext pairing traffic, though the
PIN/certificate-bound transcript still controls actual pairing. The adapter does
not invent incompatible mandatory fields to hide this protocol limitation.
Privileged access never relies on IP or uniqueid: the transport must prove TLS
possession and call `IdentityApi.verify_client` against live committed trust for
each request. Its `RequestContext.peer_trusted` represents that fresh result,
not a hostname/query claim or cached connection authorization.

There are at most eight private transcripts and 32 asynchronous service jobs,
including retired requests awaiting confirmed cleanup.
The pairing deadline is 120 seconds including PIN waiting; individual broker
requests have at most five seconds. Known owned transcripts are cancelled on
request cancellation or expiry. A cancelled begin already dispatched to the
service is drained to recover its opaque ID, then cancelled. A begin cancelled
before dispatch creates nothing. Cleanup results release runtime request tracking.
Known broker request IDs survive cancellation, result-consumption, and release
failures in a bounded retry ledger. Every 100 ms, cleanup retries cancellation and
terminal consumption/release, retaining its admission slot until release or
`not_found` confirms removal. Service operations themselves are never replayed.
Persistent broker failure therefore reduces admission capacity rather than leaking
untracked requests. Retries require module ticks: shutdown makes one best-effort
connection cancellation pass and does not run a background drain. Unresolved
broker records then rely on runtime expiry; unknown pairing transcripts rely on
the pairing authority deadline.

Delivery loss cannot roll back a consumed cryptographic phase or a committed
certificate. A transport failure before an accepted begin ID is learned can leave
an authority transcript until its own 120-second expiry. Losing a reply after it
has left the router likewise requires a fresh client attempt after cleanup/expiry;
cryptographic requests are not replayed to guess their outcome. A completed private
mapping stays only through the original deadline for final HTTPS verification.
Restart loses mappings; an administrator can inspect or unpair committed trust.

## Transport boundary and validation

`wire::Request` contains method, target, and body. `RequestContext` contains the
transport's remote IP, TLS flag, handshake-proven client certificate DER, and fresh
paired trust result. `Router::dispatch` returns an immediate reply or queues work;
`poll` yields connection/reply pairs. The server separately reports a fully flushed
reply or a premature close, and the router passes that result to launch ownership.
The transport owns HTTP framing, sockets, TLS, request deadlines, and bounded writes.
It must not promote the plaintext `clientcert` query to a TLS-authenticated identity.

Wire tests exercise parser limits and ambiguous encodings, XML escaping,
held PIN requests, replay prevention, source/certificate matching, victim-preserving
rejections, cancellation before and after begin dispatch, deadlines, and final
TLS identity requirements. Catalog tests cover authenticated admission, multipage
assembly, stable revisions, numeric ID bounds, malformed pages, visible overflow,
revocation before delivery, and retained cleanup after cancellation/release errors.
Launch tests cover signed/unsigned key IDs, exact secret parsing, mode/profile gates,
unknown and unsupported options, exported-app/RTSP sequencing, foreign cancellation,
lost-response cleanup, cleanup failure retention, and transport delivery state.
Those tests use mock brokers; real AES/RSA and TLS integration are separate gates.
The repository's `scripts/test_nvhttp.py`
performs an independent cryptographic GET transcript when run against the signed
composition.

Behavioral references were the pinned sibling's Gen7 server-info/pairing behavior
and the primary Moonlight Qt client in `app/backend/nvpairingmanager.cpp`,
`nvhttp.cpp`, and `nvcomputer.cpp`. The implementation does not copy or translate
sibling source. Gen6/SHA-1, discovery, self-service unpair,
resume, self-service unpair, HDR, surround audio, display optimization, and controller
input forwarding remain unimplemented here.

The enabled transport accepts `:0` for ephemeral ports; non-loopback binding
requires `allow_remote:true`. Its control status reports `enabled`, actual
`http_address`/`https_address`, `tls_ready`, `connections`, and fixed TLS status
codes (`identity_unavailable`, `identity_refresh_pending`, or
`tls_configuration_failed`). HTTP may serve before an administrator initializes
the identity; HTTPS becomes ready only after identity material is available.
Identity refresh reads existing state and never generates a private key.

TLS uses rustls 0.23 with the ring provider, TLS 1.2/1.3, no session resumption,
and no early data. Client certificates are optional for public pairing, but
certificate possession is verified cryptographically during the TLS handshake.
DER parsing includes supported legacy X.509 v1 certificates. RSA SHA-2 signature
verification checks the TLS transcript and exact public key; PKCS#1 signatures
are confined to TLS 1.2, with PSS available for TLS 1.2/1.3. Live pairing trust is
checked separately for each request. Host signing uses the native identity API;
the host private key is never exported to this transport. An active bridge gate
prevents rustls signer clones from calling an identity binding after teardown.
Certificate rotation rebuilds TLS configuration and closes older TLS connections.

The single tick thread allows at most 32 connections and eight per source IP,
four accepts per listener per tick, and 16 KiB of per-connection I/O per tick.
Requests are bounded to 32 KiB and 32 HTTP headers; replies to 1 MiB. Duplicate
Content-Length, chunked transfer encoding, and pipelining are rejected. Connections
close after one HTTP request. Handshake/header/write deadlines are ten seconds;
queued routing allows 130 seconds to accommodate the wire's 120-second PIN wait.
There are no detached I/O workers. Transport tests separately cover framing,
real local TCP, deferred deadlines, reply backpressure, legacy certificate
signatures, wrong keys/transcripts/schemes, and native signer teardown. Windows
and Linux workspace checks pass using the genuine SDK and C toolchains documented
in `docs/cross-compilation.md`; compilation alone does not qualify hardware capture
or a real Moonlight client.

`scripts/test_nvhttp.py` additionally exercises the signed libraries through real
HTTP/TLS with an independent Python cryptographic client. It verifies PIN waiting,
all Gen7 phases, exact peer-certificate pinning, TLS 1.2/1.3, foreign client rejection,
and pairing/revocation across restarts. The test client explicitly accepts legacy
v1 certificate metadata while retaining certificate/signature verification and an
exact DER pin; it never accepts an arbitrary server certificate.

The signed HTTP/TLS integration also covers 40 catalog entries across pages,
escaped titles, rename/delete/new IDs, TLS 1.2/1.3 app lists, stable IDs after
restart, and denial of untrusted or revoked clients.

`scripts/test_public_launch.py` exercises the signed exported-app, RTSP, stream
session, and media composition through real paired HTTPS. It is the integration
gate for launch XML, encrypted RTSP negotiation, UDP discovery, ownership, and
cleanup behavior; the startup advertisement remains a configuration declaration
rather than a replacement for that probe.
