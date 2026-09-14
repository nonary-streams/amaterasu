# Independently published frontend

The management library serves a signed asset bundle from its startup `web`
settings (`directory` and `trusted_keys`). The host shell does not interpret HTML
or own frontend activation. The API remains disabled until management `enabled`
is explicitly set; development initialization provisions the web directory and keys.

Build and publish locally:

```sh
npm --prefix frontend ci
npm --prefix frontend run build
cargo run -p amaterasu-release -- pack-web /path/to/root frontend/dist /tmp/web-1 1
cargo run -p amaterasu-release -- stage-web /path/to/root /tmp/web-1
```

`pack-web` uses the development root's signing key and gives metadata a seven-day
expiry. This is a local development publisher, not the production online updater.
The final manifest identifies `amaterasu.web`, release sequence, expiry, and every
asset's SHA-256 hash and byte size. Staging always verifies an authorized Ed25519
signature, including when native development packages permit unsigned code.
Native and web manifests are different formats and project identities.

Staging holds a separate web-root lock, writes a verified immutable bundle, then
atomically selects it. It rejects sequence rollback and same-sequence substitution.
It can run while the native supervisor holds its own root lock. The next page
request observes selection; no host restart or binary change is needed.

The management library verifies and retains asset bytes before serving them. It
injects a digest-specific base URL into `index.html`, whose script/style links are
relative. An already loaded page can finish requesting assets from its original
version while new pages use the new version. Up to four verified bundles are
cached; other retained versions are reverified on demand. Expired metadata fails
closed. Historical files are retained on disk; garbage collection and production
key rotation remain pending.

Only manifest-listed bounded paths and supported static content types are served.
Links and special files are rejected during packaging and verification. The HTTP
boundary checks the exact bound Host and approved browser Origin; its own HTTP
origin is permitted. Static responses carry a restrictive Content Security Policy,
no-sniff, same-origin resource policy, and no-store. Credentials remain subject to
API bearer and scope checks. These protections do not sandbox trusted native code
or defend against an administrator replacing the entire installed trust root.

`scripts/test_web_bundles.py` checks live activation, old-version asset consistency,
unchanged worker and shell, browser headers, and rejected tampering/rollback.
