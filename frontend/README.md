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

- Administrator username/password login or existing scoped API token sign-in.
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
- Settings forms use the registered schema's boolean, integer, and string fields;
  updates include expected revision. Empty/unregistered schemas explain what is
  missing. Integer editing is limited to JavaScript's safe integer range.
- Session pagination, stop requests, and explicit finish-stop actions. No client
  connection or streaming state is fabricated.
- Process inventory and termination with confirmation, plus explicit status refresh.
- Steam provider refresh, discovery pages, partial-scan warnings, and scan status.
- Diagnostic records, bounded journal pagination, retention warnings, and counters.

All surfaces read the management principal's shared namespace; this is not a
multi-tenant frontend. External changes can invalidate cursors and revisions;
refresh loads current state. Most lists refresh explicitly, rather than polling
silently. Known login scopes disable unauthorized mutation controls. API token
scopes are not exposed by a separate verification route; pasted token syntax is
checked locally and the server authenticates every subsequent request.

## Browser security and accessibility

Bearer tokens exist only in the API client's memory. There is no localStorage,
sessionStorage, IndexedDB, cookie auth, credential logging, URL token, or persistent
login cache. Reload, sign-out and page departure clear the token; restoring from
browser back/forward cache returns to login. Sign-out clears the browser session
only: this API currently does not expose session revocation, so the authentication
service retains it until expiration or trusted-console revocation.

Requests use POST JSON, Authorization Bearer, omitted cookies, no-store caching,
and a 12-second browser timeout. Mutations are never automatically retried;
ambiguous timeout messages instruct refresh/reconciliation. Server validation,
Origin policy and scopes remain authoritative. All dynamic content is inserted
with DOM text nodes/property setters, never HTML strings or inline event handlers.
The bundle works without inline scripts or styles.

The UI uses semantic forms, labels, headings, tables, native confirmation dialogs,
keyboard-visible focus, status/alert regions, a skip link, and responsive navigation.
Reduced-motion preferences disable transitions. Error and empty states reflect
actual API outcomes.

## Not implemented

Pairing, screen/audio capture, streaming connections, gamepad controls, legacy
client compatibility, application launching UX, Steam import/artwork, schema
registration/migration UX, credential/token lifecycle administration, multi-user
isolation, remote TLS setup, and installed-service setup remain unavailable.
These are not represented as working controls. Native module and HTTP protocol
limitations are documented by their respective READMEs.

Tests exercise literal argument handling, invalid form values, safe numeric
editing, non-exact page counts, bearer transport/clearing and domain conflict
handling without retries. Browser visual and end-to-end checks run against the
real signed frontend and management host.
