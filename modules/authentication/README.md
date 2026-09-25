# Authentication authority

`amaterasu-authentication` serves `amaterasu.authentication/1`. It provides one
administrator password credential, short-lived opaque bearer sessions, and
scoped API tokens. It is an independently built native module; HTTP routing,
TLS, cookies, CSRF protection, public network policy, and frontend identity are
separate responsibilities. Native code is trusted; runtime grants are not a
native-code sandbox.

Startup settings (administrator and scope arrays default empty, denying
administrator operations):

```json
{"administrator_principals":["amaterasu.console"],"allowed_scopes":["catalog.read","catalog.write"],"session_token_ttl_seconds":7200,"remember_me_refresh_token_ttl_seconds":604800}
```

The administrator list is restricted to 16 runtime principals. Scope names are
exact strings, maximum 64 names of 64 bytes. Access-session lifetime defaults to
7,200 seconds and remembered refresh lifetime defaults to 604,800 seconds. Both
settings accept integers from 1 through 2,147,483,647 seconds. Configured
administrator callers can read and update these lifetimes with `settings_get`
and `settings_update`; updates use an expected revision and are stored in the
caller-fixed `product/amaterasu.authentication` configuration document. The
product document uses schema version 1 and preserves packaged defaults for
known fields omitted by older partial v1 documents. Non-remembered
refresh lifetime is fixed at 86,400 seconds. There are no wildcard or implied
scopes. Deployments choose the vocabulary together with consumers. The module
needs a `provide` grant for authentication and `call` grants for
`amaterasu.state/1` and `amaterasu.configuration/1`. Consumers need
authentication call permission. State requests
preserve their parent, so current runtime delegation rules also require the
initiating consumer to possess a state call grant; configuration requests
similarly require the initiating consumer's configuration call grant. State and
product configuration isolate records by the immediate provider identity. Never
use payload caller, username, token
subject, or HTTP-supplied role claims as runtime authority.

The following strict JSON actions are supported:

```json
{"action":"setup","username":"admin","password":"a long administrator password"}
{"action":"settings_get"}
{"action":"settings_update","session_token_ttl_seconds":3600,"remember_me_refresh_token_ttl_seconds":259200,"expected_revision":0}
{"action":"login","username":"admin","password":"a long administrator password","remember_me":true}
{"action":"refresh","refresh_token":"ama_<64 lowercase hex digits>"}
{"action":"change_password","password":"a new long administrator password"}
{"action":"list_sessions","offset":0,"limit":4}
{"action":"revoke_session","session_id":"<64 lowercase hex digest digits>"}
{"action":"verify","bearer":"ama_<64 lowercase hex digits>","required_scopes":["catalog.read"]}
{"action":"logout","refresh_token":"ama_<64 lowercase hex digits>"}
{"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":3600}
{"action":"list_tokens"}
{"action":"revoke_token","token_id":"<64 lowercase hex digest digits>"}
```

Setup, lifetime settings, password rotation, session administration, token
issuance, listing, and revocation require the immediate runtime caller to appear
in administrator settings. Lifetime updates are persisted before acknowledgment
and use compare-and-swap revisions; login and refresh load the committed values
before issuing tokens. TTL edits affect subsequently issued tokens but do not
extend an existing session's absolute refresh expiry; refresh rotation also
retains that original expiry. Setup succeeds only once. A caller
can list/revoke only tokens that caller issued; missing and foreign token IDs
both return `token_not_found`. An administrator login does not grant its frontend
runtime principal permission to administer credentials or tokens. API token
issuance requires initialized credentials but is authorized by the configured
runtime principal, without password or bearer substitution.

`change_password` requires an initialized credential and the same 12–256-byte
password bounds as setup; it is an administrator reset operation and does not
require the previous password. It preserves the username and API tokens, durably
changes the password hash, and invalidates every login session after write
acknowledgment. Session authorization also checks its credential fingerprint
against loaded state, covering a committed rotation whose acknowledgment was
lost to cancellation. A failed uncommitted write leaves existing sessions valid.
Configured administrator callers can list all administrator-session metadata in
pages of 1–4 entries (default 4), with `next_offset`, and idempotently revoke a
session by `session_id`. Session IDs are digest metadata, never reusable bearer
secrets; listings omit credential fingerprints. Frontends cannot administer
sessions by merely presenting a login bearer.

## Legacy administrator migration

`amaterasu-authentication-import.exe` accepts only the current legacy
`username`, `password`, and 16-character alphanumeric `salt` record, where
`password` is lowercase SHA-256 hex of password followed by salt. It rejects
ambiguous, external, malformed, or API-token-bearing records. Its `seed` mode
writes the ordinary authentication state namespace only when empty. The retained
legacy verifier is constant-time checked at login and, after the first successful
password proof, is replaced in the same durable write with the native Argon2id
credential. Passwords and verifier values are never printed. The installer must
call `validate` before takeover and call `seed` only on the copied
`DATA_ROOT/legacy-imports/PRODUCT` source.

Successful login returns `ok`, access `bearer`, `refresh_token`,
`kind:"administrator_session"`, `subject` (the administrator username), `scopes`,
`expires_at`, `refresh_expires_at`, and `remember_me`. At login, select the
configured `remember_me_refresh_token_ttl_seconds` for remembered sessions or
the fixed 86,400 seconds otherwise. The absolute refresh expiry is login time
plus the larger of that selected refresh TTL and `session_token_ttl_seconds`.
Access expiry is login time plus `session_token_ttl_seconds`, capped at the
absolute refresh expiry. Refresh rotates both tokens and sets a new access
expiry using the current access TTL, capped at the session's unchanged absolute
refresh expiry. TTL edits alone leave stored expiries unchanged; refresh rotation
may renew access expiry but keeps the absolute refresh expiry fixed. The refresh
token is accepted only by `refresh` and `logout`; either it or the access bearer
can revoke the entire session pair. Logout is idempotent and does not revoke API
tokens. Session metadata listings include access and refresh expiration plus the
remember-me flag, but never either secret.

Administrator sessions persist in a separate versioned
`authentication/sessions/v1` state document (schema version 1), apart from
credential and API-token state at `authentication/v1`. The session document is
bounded to 16 records and 12,000 serialized bytes. It stores bearer digests,
credential and scope digests, and session metadata; it never stores raw bearer or
refresh-token secrets. On module restart, sessions restore only when their
credential and configured-scope digests still match and their absolute refresh
expiry has not passed. The administrator token lifetime settings remain durable
across module restarts. The packaged runtime profile supplies default TTL
settings, and the management API and settings editor expose administrator-facing
lifetime controls. Verify returns identity/scope/expiry metadata without the
bearer, with kind `administrator_session` or `api_token`; a
token's subject is its issuing runtime principal. All required scopes must match.
Removing a configured scope denies any session/token that still grants it,
including otherwise valid requests for a smaller subset.

Token issuance returns `ok`, the one-time `bearer`, its `token_id`, `scopes`, and
`expires_at`. The ID is the SHA-256 bearer digest and may be used for revocation;
it is not a bearer. Lifetimes range from 1 second to 30 days. Expired tokens are
rejected and pruned on issuance. At most 32 API tokens are retained, subject also
to a 12,000-byte document bound. Listing returns owned metadata including token
IDs and expiration, never bearer secrets or password hashes. Revoked token
records are removed and verification reloads state, so cached token authorization
cannot outlive acknowledged revocation.

Credentials use RustCrypto Argon2id v19, 19,456 KiB memory, two iterations, one
lane, 16 random salt bytes, and a 32-byte password hash encoded in PHC format.
Persisted hashes are validated against these parameters before verification so
corrupt state cannot request unbounded hashing work. Passwords are 12–256 UTF-8
bytes for setup and at most 256 bytes for login; usernames are 1–64 restricted
identifier bytes. Bearers contain 32 bytes from the OS random generator encoded
as lowercase hex with the `ama_` prefix. Only their SHA-256 digest is persisted.
Randomness failure denies issuance. Errors are fixed codes and do not include
passwords, hashes, tokens, or untrusted transport error text. Secrets necessarily
exist transiently in native/runtime memory; this module does not promise memory
zeroization or protection against trusted native modules or memory dumps.

Credential, token, and session operations serially load the caller-isolated
credential/API-token state document `authentication/v1` and session state
document `authentication/sessions/v1`. Setup/issue/revoke use revision-checked
state writes and complete successfully only after persistence acknowledgment.
Session document updates use revision-checked writes to that separate key.
There is no internal password request backlog; the runtime applies bounded
backpressure.
Cancelled writes may already be durable, even if delivery of the response fails.
A token bearer lost to cancellation is unrecoverable; use owned token metadata
listing to revoke it and issue a new token. Issuance is not idempotent across
separate requests. Cancelled session delivery can occupy a session slot until
expiry. State failures deny access and do not fall back to cached credentials or
API-token records.

Login admission permits at most five attempts per runtime caller per 60 seconds
and 20 attempts globally per window. The bounded in-memory rate ledger is keyed
by runtime callers, not client-supplied usernames or addresses. A shared frontend
therefore shares one budget; a public HTTP provider should apply additional
network-aware admission controls. The ledger resets on restart. Hashing is
synchronous and bounded by the specified Argon2 cost; it can briefly occupy the
module's tick. Time is monotonic-clamped within an instance. Persistent token
expiry depends on the host wall clock across restarts; this module does not offer
an independent trusted clock or rollback-resistant storage.

Errors include `invalid_request`, `forbidden`, `unauthorized`, `rate_limited`,
`already_initialized`, `not_initialized`, `token_not_found`, `token_limit`,
`session_limit`, `resource_limit`, `invalid_state`, `state_unavailable`,
`deadline_exceeded`, `entropy_unavailable`, `credential_failure`, and
`clock_failure`. Unknown fields are rejected; request JSON is bounded to 4096
serialized bytes. Error replies are `{"ok":false,"code":"..."}`.

Offline password recovery, multiple human users, durable sessions, external identity
providers, MFA, audit trails, and password breach checks remain unimplemented.
Offline state/host compromise is outside this in-process authority boundary.

Tests exercise the real Argon2 implementation, asynchronous state request/reply
ordering, retained document reconstruction, cancellation, password rotation,
credential-bound session invalidation, write failure, scope
and ownership boundaries, token digest storage, deterministic expiry, logout,
capacity, and login rate limits. The transport harness models state acknowledgment;
state disk durability and complete runtime integration require their own tests.

API references: [RustCrypto Argon2](https://docs.rs/argon2/0.5.3/argon2/),
[getrandom 0.3](https://docs.rs/getrandom/0.3.4/getrandom/), and
[RustCrypto SHA-2](https://docs.rs/sha2/0.10.9/sha2/).

Allowed policy scopes and token scope sets are bounded to 64 exact identifiers.
The development policy includes `media.read` for the fixed media-discovery routes;
it grants no native interface authority.
