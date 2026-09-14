# Authentication authority

`amaterasu-authentication` serves `amaterasu.authentication/1`. It provides one
administrator password credential, short-lived opaque bearer sessions, and
scoped API tokens. It is an independently built native module; HTTP routing,
TLS, cookies, CSRF protection, public network policy, and frontend identity are
separate responsibilities. Native code is trusted; runtime grants are not a
native-code sandbox.

Startup settings (both arrays default empty, denying administrator operations):

```json
{"administrator_principals":["amaterasu.console"],"allowed_scopes":["catalog.read","catalog.write"]}
```

The administrator list is restricted to 16 runtime principals. Scope names are
exact strings, maximum 16 names of 64 bytes. There are no wildcard or implied
scopes. Deployments choose the vocabulary together with consumers. The module
needs a `provide` grant for authentication and a `call` grant for
`amaterasu.state/1`. Consumers need authentication call permission. State requests
preserve their parent, so current runtime delegation rules also require the
initiating consumer to possess a state call grant; state still isolates records
by the immediate provider identity. Never use payload caller, username, token
subject, or HTTP-supplied role claims as runtime authority.

The following strict JSON actions are supported:

```json
{"action":"setup","username":"admin","password":"a long administrator password"}
{"action":"login","username":"admin","password":"a long administrator password"}
{"action":"change_password","password":"a new long administrator password"}
{"action":"list_sessions","offset":0,"limit":4}
{"action":"revoke_session","session_id":"<64 lowercase hex digest digits>"}
{"action":"verify","bearer":"ama_<64 lowercase hex digits>","required_scopes":["catalog.read"]}
{"action":"logout","bearer":"ama_<64 lowercase hex digits>"}
{"action":"issue_token","scopes":["catalog.read"],"ttl_seconds":3600}
{"action":"list_tokens"}
{"action":"revoke_token","token_id":"<64 lowercase hex digest digits>"}
```

Setup, password rotation, session administration, token issuance, listing, and
revocation require the immediate runtime
caller to appear in administrator settings. Setup succeeds only once. A caller
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

Successful login returns `ok`, `bearer`, `kind:"administrator_session"`, `subject`
(the administrator username), `scopes`, and `expires_at` (Unix seconds). Sessions
receive the configured scope set, last 900 seconds, and are held only in memory
under their SHA-256 digest. There are at most 128 active sessions. Logout removes
a session, is idempotent, and does not revoke API tokens. Module restart invalidates
sessions. Verify returns the same identity/scope/expiry metadata without the
bearer, with kind `administrator_session` or `api_token`; a token's subject is its
issuing runtime principal. All required scopes must match. Removing a configured
scope denies any session/token that still grants it, including otherwise valid
requests for a smaller subset.

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

Each operation serially loads the caller-isolated state document
`authentication/v1`. Setup/issue/revoke then use a revision-checked state write
and complete successfully only after persistence acknowledgment. There is no
internal password request backlog; the runtime applies bounded backpressure.
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
