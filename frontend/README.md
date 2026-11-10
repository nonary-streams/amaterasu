# Amaterasu web console

An independently replaceable frontend for the versioned management HTTP API.
It has no runtime dependencies, CDN requests, embedded fonts, or service worker.
The only project dependency is a pinned TypeScript compiler for development.

## Build and verify

From this directory, with Node 22 or newer and npm:

```sh
npm ci --ignore-scripts
npm run typecheck
npm test
npm run build
```

Run the opt-in browser session qualifications separately:

```sh
node tests/browser-session-qualification.mjs
node tests/browser-cookie-qualification.mjs
node tests/integrations.browser-qualification.mjs
```

These tests use temporary browser profiles and ephemeral loopback listeners.
The integration dashboard qualification runs the actual frontend modules in Chromium
and uses mocked provider API responses; it verifies browser rendering, focus, locale
updates and detached-page cleanup, not live host integration. They default to Chromium
on Linux, Microsoft Edge on Windows, and Google Chrome on macOS. Set
`AMATERASU_BROWSER` to another compatible browser executable when needed. The HTTPS test skips certificate verification in its isolated browser
process for the local fixture. Its server is a controlled authentication fixture; these
checks qualify browser cookie, reload, refresh, and peer-tab behavior, while the
signed management host tests qualify the real authentication service.

From the workspace root, `python3 scripts/test_signed_browser_session.py` runs
Chromium against a freshly signed native HTTPS host and signed production
assets. It requires a normal
Linux user, Chromium, Node with built-in WebSocket support, Python `cryptography`,
and the usual native build tools. Set `AMATERASU_BROWSER` to select the Chromium
executable, or use `--skip-build` after building the workspace and frontend.
All listeners use temporary loopback ports; host state and the browser's HOME,
XDG directories and profile stay in a private temporary directory.

This gate exercises actual UI remembered login and page reload, HttpOnly cookie
secrecy, CSRF denial, native refresh rotation and administrative revocation,
policy save and stale-revision conflict, native host restart persistence, and a
cold Chromium process restart with the same profile. Before closing Chromium,
the gate removes the session-only access cookie and confirms that the persistent
remembered cookie restores the signed-in UI in the new process. It also checks
peer-tab sign-out. An injected post-admission failure also checks browser helper
cleanup through retained process descriptors. Chromium receives a certificate
exception for only the newly generated SPKI key; an unrelated untrusted key
must still fail browser verification, and Python independently verifies the
host certificate and loopback IP SAN. No CA, installed browser preference or
service is changed. This qualifies the controlled Linux host/browser path;
installed-service trust, physical platform TLS provisioning, and other browsers
remain separate gates.

The deterministic output is exactly `dist/index.html`, `dist/app.js`, and
`dist/styles.css`. Built assets are included for the native package builder;
`node_modules` is not. Source files are small TypeScript namespace modules compiled
into one script, with no bundler/runtime module loader. The Rust management module
owns signed bundle verification and HTTP delivery; this frontend never writes or
updates native modules.

Asset references are relative. The management server may inject a base URL such
as `/web/<verified-bundle-digest>/` so a loaded page uses one immutable bundle.
API requests always use absolute `/v1/...` paths. The `amaterasu-api-base` meta tag
defaults to the current origin; an explicit HTTP(S) origin can be supplied when
building a separately hosted frontend. That origin must permit this browser in
its configured Origin policy. Opening `index.html` directly from `file:` is not
supported. The native server must allow its own exact Origin for same-origin
browser API calls.

## Implemented workflows

- Administrator username/password login with an optional remembered session, or
  existing scoped API token sign-in. A remembered session is restored before the
  authenticated shell appears and when the browser returns the page from its
  back/forward cache. Administrator sessions silently rotate their access and
  refresh pair as needed for requests while the tab remains open.
  First-run enrollment can create the first administrator in the browser using
  the enrollment code shown locally on the host.
- Overview shows actual responses from catalog, sessions, applications, and Steam,
  host-reported stream activity, and measured host vitals when enabled and permitted.
  Active stream counts come from the stream owner and refresh every 10 seconds
  while the page is visible. Pending launches come from the history tracker and
  can be unavailable independently. Service responses and activity do not confirm
  client media delivery.
  Missing authority or failed services appear unavailable. Counts ending in `+`
  are bounded page counts, not invented global totals.
- Integrations shows the reported status of Steam, Lutris, and Playnite and links
  to their settings. Provider availability is based on the host's status reply,
  not inferred from library counts.
- Host & devices shows pending pairing requests, allows an administrator with
  `pairing.admin` to submit a four-digit device PIN, and reports host listeners,
  streaming history health, and management sessions using their existing read
  scopes. PIN input is cleared on submission and is not stored or rendered as
  text. Approval advances the pairing protocol; it does not claim the device
  completed pairing. If an approval response is interrupted, the page refreshes
  current request status without replaying the PIN mutation.
- Catalog list/search with revision-bound next/previous pagination, including empty
  search pages with valid continuations. Create, edit, and delete use expected
  catalog revisions; conflicts require refresh and are never retried silently.
  Search includes launch targets and linked sources. Selection persists across pages
  at the same revision; Space toggles an item, Shift with an arrow extends a visible
  range, and Escape clears selection. Desktop and Playnite Fullscreen edits preserve
  their process-less launch settings and preparation steps.
  Global name and known-source sorting first scans the matching catalog into a compact
  revision-bound index, then loads only visible full entries. This can take time for
  large libraries. Entries without source metadata are grouped as Other; provider
  provenance is not currently available for all entries.
  Application/executable paths, literal JSON argument arrays, description and tags
  are editable. Editing catalog metadata does not launch applications; the separate
  Launch action is available to accounts with launch authority.
  Command output uses an unset/value choice so empty strings and the literal
  `null` remain distinct from an omitted destination. Saved text is not trimmed;
  relative paths and environment references are retained for launch-time
  resolution. Existing raw preparation commands are carried through manual edits
  as bounded, validated hidden metadata without changing their command text.
  The destination covers application, preparation, and detached commands;
  externally provider-owned game output is not routed through this setting.
- Settings search matches labels, hints and keys across loaded categories while preserving
  the original editor forms, save actions and unsaved values.
- Settings forms use the registered schema's boolean, integer, decimal, string,
  enum, list, and object fields; updates include expected revision. List and object
  values use JSON editors, with server-side nested validation. Empty/unregistered
  schemas explain what is missing. Integer editing is limited to JavaScript's safe
  integer range. The Moonlight name and HTTP port panel uses a separate product
  settings route with its own revision. Accepted changes update live server
  information and listener ports; the HTTPS port is five below the HTTP port.
  Steam settings control bounded discovery and catalog synchronization, with
  provider status and opaque artwork readback.
- Paired-device removal requires confirmation, uses a revision-checked write, and
  confirms the remaining client list without discarding other device drafts. When
  a save or removal conflicts, reviewing the latest revision preserves compatible
  drafts; competing edits are shown and remain unsaved until resolved.
- Management access and CSRF origin exception forms share the management policy
  revision and confirm durable readback after saving. CSRF exceptions default to
  none; default/null and an explicit empty list remain distinct. Exact trusted
  HTTPS origins relax only the cookie CSRF predicate, with separate browser
  access policy, authentication and route scopes still required.
- Session pagination, stop requests, and explicit finish-stop actions. No client
  connection or streaming state is fabricated.
- Sessions allows an authorized operator to stop the current NVHTTP-managed
  launch after confirmation. The request is bound to the observed launch and
  reconnect state; it ends the associated application as well as the stream.
  Admission is followed by cleanup readback, with a retry for failed cleanup.
  Streams owned outside NVHTTP are not controlled by this action.
- Sessions shows per-stream host-side performance cards for active streams
  even when history recording is disabled. Live rates come from successive host
  counter snapshots and remain in the browser; no rate is invented for the first
  reading. When available, the cards show recent recorded samples immediately,
  then load retained samples within the configured point and time limits. They use
  row identities to update the bounded window and preserve the selected chart range
  across refreshes. Combined charts append only live intervals that begin after
  the captured recorded data, with separate line styles; live-only and recorded-only
  views are also available. A failed longer-history read leaves the live view available.
  Host telemetry and stream-data failures are independent. The charts do not report
  client delivery. Adjacent recorded streams with matching client, device, application and protocol
  and a gap of at most two minutes are grouped on the current page as possible
  reconnects. Group charts keep stream segments separate, member actions remain
  individual, and grouped JSON preserves each member's snapshot. Signed history
  runtime status is shown independently from the recorded session listing:
  configuration readiness, effective recording, storage failures, counts,
  and the most recent retention result. Status can be refreshed without hiding the
  session list when its request fails. Recorded-session details chart returned
  host-side rates, sequence-gap counters, and measured encoder latency where present;
  range filters and a JSON download operate on a captured retained snapshot. Recent
  data is shown while full history loads; changed or excessively large snapshots
  leave the recent view available with an explanation. Local transport
  acceptance does not establish client delivery.
- Process inventory and termination with confirmation, plus explicit status refresh.
- Steam provider refresh, discovery pages, partial-scan warnings, and scan status.
- Diagnostics provides search, match navigation and export for the current
  host log when `host-logs.read` is granted. A level filter and search narrow
  the preview; search and the plain-text download use the full retained current-session snapshot.
  A separate sanitized support bundle includes retained sessions from the current log
  location, with size limits and truncation notices; review it before sharing. The separate
  caller-scoped journal has its own search, copy, export and retention warnings.
  The host log follows new bytes every five seconds while visible, can be paused,
  and keeps the displayed snapshot stable while searching, selecting text or
  reading away from the end. Show latest applies accumulated updates.
- Maintenance shows signed-in sessions to authorized administrators and can revoke
  another session with readback. It reports only available account and expiry
  information; device names and last activity are not recorded.
- API tokens lets administrator sessions with `tokens.manage` select named
  permissions and an expiry, create and revoke browser-issued credentials, and
  copy the secret once. It does not display native-caller or legacy tokens, and
  never stores the secret in browser storage. An uncertain issuance is not retried.
- Plugins has three views, each addressable as `#plugins`, `#plugins/available` and
  `#plugins/repositories`. Installed lists the modules in the package the host reports
  as loaded, with each module's declared dependencies and services; this does not
  establish individual module health. Available compares what the approved repositories
  offer with what is loaded. Repositories lists publishers and their catalog status, and
  lets an administrator session add one (publisher key plus a separately confirmed
  fingerprint), check one for updates, or remove one. The page does not install, stage
  or activate anything: see `docs/approved-repositories.md`. The old `#updates` address
  still opens it.
- Host logging settings confirm the saved revision and separately read signed
  diagnostics status. The editor distinguishes pending application from healthy,
  disabled, and failed disk delivery; older status replies leave delivery unverified.
  Sink failures report only the operation category, without exposing disk paths.

The shared input editor preserves ordered keybindings at the host, application,
and certificate-bound client scopes. Scoped blank text inherits; `[]` clears
lower custom mappings while retaining built-in modifiers and the optional
Right Alt mapping. Decimal and hexadecimal text is sent as exact integer JSON
bytes, with at most 256 pairs. This 0..255 domain is narrower than Vibeshine's
arbitrary integer parser. The native input module applies the selected mappings
only where its platform backend supports the destination key; browser tests and
synthetic input probes do not qualify operating-system key delivery.

All surfaces read the management principal's shared namespace; this is not a
multi-tenant frontend. External changes can invalidate cursors and revisions;
refresh loads current state. Most lists refresh explicitly, rather than polling
silently. Known login scopes disable unauthorized mutation controls. API token
scopes are not exposed by a separate verification route; pasted token syntax is
checked locally and the server authenticates every subsequent request.

## Browser security and accessibility

Same-origin HTTPS administrator login uses Secure, HttpOnly, SameSite=Strict
cookies. Script receives session metadata rather than access or refresh tokens.
Cookie requests use same-origin credentials and the required CSRF header;
browser-origin checks and authorization scopes remain enforced by management.
Web Locks serialize login, refresh, and sign-out across tabs. Local storage holds
sign-out coordination markers, without cookie token secrets. Remember this
browser selects the longer refresh lifetime and a persistent refresh cookie.

Plain HTTP and separately hosted API origins use bearer authentication instead.
Non-remembered tokens stay in memory. Remembered bearer sessions store the raw
access and refresh pair in one localStorage record, readable by same-origin
scripts, and require Web Locks for safe coordination. Without Web Locks this
fallback can sign in with Remember this browser unchecked. Scoped API-token
sign-in stays memory-only and uses bearer transport even on HTTPS. The backend
persists token digests. Native modules are trusted code; API grants and browser
session handling do not sandbox native code.

Page departure clears in-memory session state. Reload and back/forward-cache
return restore an eligible cookie or remembered bearer session before showing
the authenticated shell. An ambiguous refresh is not replayed. Cookie sign-out
retains a local pending marker until server revocation is confirmed, and peer
tabs observe sign-out through shared coordination. Bearer sign-out removes its
remembered pair under the refresh lock and attempts server revocation. Signing
out with an API token clears local use without revoking the token.

Requests use POST JSON, no-store caching, and a 12-second browser timeout.
Bearer requests omit cookies. An explicit unauthorized response may cause one
retry after management verifies a rotated session; timeouts and transport failures
never retry mutations. Ambiguous timeout messages instruct refresh/reconciliation.
Server validation, Origin policy and scopes remain authoritative. All dynamic
content is inserted with DOM text nodes/property setters, never HTML strings or
inline event handlers. The bundle works without inline scripts or styles.

The UI uses semantic forms, labels, headings, tables, native confirmation dialogs,
keyboard-visible focus, status/alert regions, a skip link, and responsive navigation.
Light, dark, and system appearance choices are stored as a non-secret local browser
preference. Library application editors, Settings, Accounts, and paired-client
editors warn before navigating away from changed values. Settings refresh is
blocked while edits are pending. A failed or conflicting paired-client save keeps
all client drafts visible and disables further saves until an explicit reload;
that reload discards those drafts after confirmation. These protections do not
replace server revision checks or guarantee recovery after authentication loss.
Reduced-motion preferences disable transitions. Error and empty states reflect
actual API outcomes.

## Not implemented

The console edits host, application, and paired-client settings, but does not
initiate streaming sessions or qualify native capture and input delivery. Remote
TLS provisioning and installed-service setup have no browser workflow.
All management surfaces share one administrative namespace. Native module and
HTTP protocol limitations are documented by their respective READMEs.

Tests exercise literal argument handling, invalid form values, safe numeric
editing, non-exact page counts, bearer transport/clearing, single-flight refresh,
token rotation, logout revocation and domain conflict handling. The signed-host browser gate exercises authentication and selected management
workflows; fixture-based visual checks do not qualify installed-service or remote
browser behavior.
