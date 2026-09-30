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
```

These tests use temporary browser profiles and ephemeral loopback listeners.
They default to Chromium on Linux, Microsoft Edge on Windows, and Google Chrome
on macOS. Set `AMATERASU_BROWSER` to another compatible browser executable when
needed. The HTTPS test skips certificate verification in its isolated browser
process for the local fixture. Its server is a controlled authentication fixture; these
checks qualify browser cookie, reload, refresh, and peer-tab behavior, while the
signed management host tests qualify the real authentication service.

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
- Catalog list/search with revision-bound next/previous pagination, including empty
  search pages with valid continuations. Create, edit, and delete use expected
  catalog revisions; conflicts require refresh and are never retried silently.
  Application/executable paths, literal JSON argument arrays, description and tags
  are editable. Catalog operations do not launch applications.
  Command output uses an unset/value choice so empty strings and the literal
  `null` remain distinct from an omitted destination. Saved text is not trimmed;
  relative paths and environment references are retained for launch-time
  resolution. Existing raw preparation commands are carried through manual edits
  as bounded, validated hidden metadata without changing their command text.
  The destination covers application, preparation, and detached commands;
  externally provider-owned game output is not routed through this setting.
- Settings forms use the registered schema's boolean, integer, decimal, string,
  enum, list, and object fields; updates include expected revision. List and object
  values use JSON editors, with server-side nested validation. Empty/unregistered
  schemas explain what is missing. Integer editing is limited to JavaScript's safe
  integer range. The Moonlight name and HTTP port panel uses a separate product
  settings route with its own revision. Accepted changes update live server
  information and listener ports; the HTTPS port is five below the HTTP port.
  Steam settings control bounded discovery and catalog synchronization, with
  provider status and opaque artwork readback.
- Management access and CSRF origin exception forms share the management policy
  revision and confirm durable readback after saving. CSRF exceptions default to
  none; default/null and an explicit empty list remain distinct. Exact trusted
  HTTPS origins relax only the cookie CSRF predicate, with separate browser
  access policy, authentication and route scopes still required.
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
token rotation, logout revocation and domain conflict handling. Browser visual and
end-to-end checks run against the real signed frontend and management host.
