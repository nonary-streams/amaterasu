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
  First-run instructions explain that initial credentials must be created through
  the trusted local console. There is no browser password bootstrap.
- Overview shows actual responses from catalog, sessions, applications, and Steam.
  Missing authority or failed services appear unavailable. Counts ending in `+`
  are bounded page counts, not invented global totals.
- Host & devices shows pending pairing requests, allows an administrator with
  `pairing.admin` to submit a four-digit device PIN, and reports host listeners,
  streaming history health, and management sessions using their existing read
  scopes. PIN input is cleared on submission and is not stored or rendered as
  text. Approval advances the pairing protocol; it does not claim the device
  completed pairing.
- Catalog list/search with revision-bound next/previous pagination, including empty
  search pages with valid continuations. Create, edit, and delete use expected
  catalog revisions; conflicts require refresh and are never retried silently.
  Application/executable paths, literal JSON argument arrays, description and tags
  are editable. Catalog operations do not launch applications.
- Settings forms use the registered schema's boolean, integer, decimal, string,
  enum, list, and object fields; updates include expected revision. List and object
  values use JSON editors, with server-side nested validation. Empty/unregistered
  schemas explain what is missing. Integer editing is limited to JavaScript's safe
  integer range. The Moonlight name and HTTP port panel uses a separate product
  settings route with its own revision. Accepted changes update live server
  information and listener ports; the HTTPS port is five below the HTTP port.
  A separate Steam discovery toggle controls explicit local
  library scans; synchronization with the application catalog remains unavailable.
- Session pagination, stop requests, and explicit finish-stop actions. No client
  connection or streaming state is fabricated.
- Sessions shows signed history runtime status independently from recorded session
  listing: configuration readiness, effective recording, storage failures, counts,
  and the most recent retention result. Status can be refreshed without hiding the
  session list when its request fails.
- Process inventory and termination with confirmation, plus explicit status refresh.
- Steam provider refresh, discovery pages, partial-scan warnings, and scan status.
- Diagnostic records, bounded journal pagination, retention warnings, and counters.
- Host logging settings confirm the saved revision and separately read signed
  diagnostics status. The editor distinguishes pending application from healthy,
  disabled, and failed disk delivery; older status replies leave delivery unverified.
  Sink failures report only the operation category, without exposing disk paths.

All surfaces read the management principal's shared namespace; this is not a
multi-tenant frontend. External changes can invalidate cursors and revisions;
refresh loads current state. Most lists refresh explicitly, rather than polling
silently. Known login scopes disable unauthorized mutation controls. API token
scopes are not exposed by a separate verification route; pasted token syntax is
checked locally and the server authenticates every subsequent request.

## Browser security and accessibility

Non-remembered administrator bearer tokens exist only in the API client's memory.
Scoped API-token sign-in also stays memory-only and is never written to the
remembered administrator-session record. With Remember this browser enabled, the
raw administrator access and refresh token pair is stored together in one browser
`localStorage` record. That record persists across page reloads and browser
restarts, subject to the refresh token's server expiry. Same-origin scripts can
read browser storage, so this is not HttpOnly protection. Native modules are
trusted code; browser token storage is not a native-code sandbox. The backend
persists token digests only, never the raw token values. Remembered administrator
  sessions can survive a management-module restart while the refresh token remains
  valid. Remember me requires Web Locks in a secure browser context so storage
  updates and refreshes stay serialized across tabs. If Web Locks are unavailable,
  sign in with Remember this browser unchecked.

Page departure clears in-memory credentials while preserving a valid remembered
pair. Returning through the browser back/forward cache runs the same remembered
session restore flow before showing the authenticated shell. If a refresh is
ambiguous during suspension, it is not replayed. Sign-out clears in-memory
credentials immediately, then removes the matching remembered pair under the
same lock used for refresh and login. It sends a best-effort revocation using the
refresh token. If the page is reloaded or closed before revocation completes, the
server session remains valid until its absolute refresh expiry or
trusted-administrator revocation. Signing out with an API token does not revoke
that token.

Remember me also selects the configured longer refresh lifetime. Without it, the
browser keeps administrator session tokens in memory only.

Requests use POST JSON, Authorization Bearer, omitted cookies, no-store caching,
and a 12-second browser timeout. An explicit unauthorized response may cause one
retry after management verifies a rotated session; timeouts and transport failures
never retry mutations. Ambiguous timeout messages instruct refresh/reconciliation.
Server validation, Origin policy and scopes remain authoritative. All dynamic
content is inserted with DOM text nodes/property setters, never HTML strings or
inline event handlers. The bundle works without inline scripts or styles.

The UI uses semantic forms, labels, headings, tables, native confirmation dialogs,
keyboard-visible focus, status/alert regions, a skip link, and responsive navigation.
Reduced-motion preferences disable transitions. Error and empty states reflect
actual API outcomes.

## Not implemented

Screen/audio capture, streaming connections, gamepad controls, legacy
client compatibility, application launching UX, Steam import/artwork, schema
registration/migration UX, credential/token lifecycle administration, multi-user
isolation, remote TLS setup, and installed-service setup remain unavailable.
These are not represented as working controls. Native module and HTTP protocol
limitations are documented by their respective READMEs.

Tests exercise literal argument handling, invalid form values, safe numeric
editing, non-exact page counts, bearer transport/clearing, single-flight refresh,
token rotation, logout revocation and domain conflict handling. Browser visual and
end-to-end checks run against the real signed frontend and management host.
