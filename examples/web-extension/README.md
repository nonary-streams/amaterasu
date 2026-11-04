# Hello web extension

This example registers a panel using the version 1 browser SDK. The source
folder passed to `pack` must contain only supported web assets; copy
`example.js` into a separate directory before packaging. Keep the publisher's
32-byte Ed25519 secret, encoded as 64 hexadecimal characters, in a private file.

The host calls `mount(container, api)` with `api.version`, an `AbortSignal` in
`api.signal`, and `api.call(alias, payload)`. Calls are limited to aliases
declared in the signed manifest and approved by the host. `mount` may return a
cleanup function, which the host invokes on navigation or signout.
The additive `api.ui` v1 contract supplies reusable accessible controls; see
[the UI SDK contract](../../docs/web-extension-ui-sdk.md) and the standalone
[controls consumer](controls.js).

```sh
mkdir -p /tmp/hello-source
cp examples/web-extension/example.js /tmp/hello-source/
cargo run -p amaterasu-package --bin amaterasu-web-extension -- \
  pack /tmp/hello-source /tmp/hello-bundle hello 'Hello' example.js \
  1 EXPIRY_UNIX /path/to/private-key examples/web-extension/methods.json
```

Replace `EXPIRY_UNIX` with a future Unix timestamp. The command prints the
publisher fingerprint and public key. Verify these through a trusted channel
before approving them locally. Stage the signed bundle in a dedicated directory
whose final component is the signed extension ID:

```sh
cargo run -p amaterasu-package --bin amaterasu-web-extension -- \
  stage /tmp/hello-bundle /path/to/extensions/hello FINGERPRINT PUBLIC_KEY_HEX
```

To rotate a publisher key, explicitly approve the new key and stage a release
with a higher sequence, supplying the previous publisher fingerprint and public
key as two additional arguments. The existing selected release must verify with
an approved supplied key. Preserve the selection directory: its release floor
prevents older or conflicting releases even across publishers. Deleting or
rolling back that local state removes this protection. Configure management's
local extension approval separately before the UI will expose the extension.

A pack command can take a JSON file containing a list of method declarations
after the private key path. Each declaration has `alias`,
`provider`, `service`, `method`, `interface_major`, `interface_minor`, `scope`,
and `min_role`. These declarations are signed; local authorization and the
server's own access checks still apply.

Extensions execute as trusted browser JavaScript, not in a security sandbox.
Only approve code and publishers trusted with the browser session. Native
modules are outside this extension mechanism.

## Run the greeting example

The `greet` example needs the opt-in native fixture in
`fixtures/extension-example`, as well as the signed browser extension. It is a
sample provider and is not included in the ordinary Linux session package. For
a self-contained, automated loopback exercise of the signed fixture, broker,
authentication, and role checks, run from the repository root:

```sh
python3 scripts/test_network_accounts_extensions.py
```

To use the example in a development host with the browser UI, first initialize
an isolated root using the management setup instructions in
`docs/linux-session-profile.md`. Build the workspace, including the host,
publisher and `amaterasu-extension-example`, and build the UI with
`npm --prefix frontend ci` and `npm --prefix frontend run build`. With absolute
paths substituted below and the host stopped, stage the fixture profile and web
bundle:

```sh
amaterasu-release pack-extension-example ROOT ARTIFACTS NATIVE_PACKAGE 1
amaterasu-release stage ROOT NATIVE_PACKAGE
amaterasu-release pack-web ROOT frontend/dist WEB_PACKAGE 1
amaterasu-release stage-web ROOT WEB_PACKAGE
```

`ARTIFACTS` is the built workspace's `target/debug` directory (or the matching
release directory). Use fresh package output paths. This opt-in fixture profile
is for isolated testing and is not the Linux capture profile. Configure the
management listener, TLS and enrollment as described in the Linux management
setup documentation; the initializer's management port option enables a
loopback listener. Stage the extension as above before starting the host.

While the host is stopped, configure its `bootstrap.json` policy. The adjacent
`policy-example.json` is a **fragment of the `policy` object**, not a complete
bootstrap file. Merge its entries into the existing policy without replacing
unrelated settings, principals, call grants, or API approvals:

- Add its `settings.amaterasu.management.extensions` entry, substituting an
  absolute staged extension root, the fingerprint, and public key printed by
  `pack`. The entry ID must equal the signed `hello` ID and be unique. Exactly
  one current key is accepted for each configured entry.
- Add the fixture principal's `provide` grant, add `example.extension/1` to
  the existing management principal's `call` list, and append the exact
  `api_methods` approval. The fixture must also be present and running in the
  selected native package. Local approval alone cannot install a provider or
  make the broker accept its calls.
- Confirm `catalog.read` is present in authentication's `allowed_scopes`.
  Increment `authorization_policy_generation` when changing allowed scopes or
  administrator principals, including when reverting such changes. The account's
  granted scopes are intersected with that policy. The signed and locally
  approved declaration additionally requires at least the `operator` role;
  a viewer cannot call it even if the viewer has `catalog.read`.

Pass `examples/web-extension/methods.json` as the final argument to `pack` so
that the declared method is signed. The local `methods` entry must match every
field in that declaration. Start the host, sign in to the management UI as an
operator or administrator, open **Hello**, and select **Say hello**. With the
fixture running and all grants in place, the panel displays `Hello, Amaterasu!`.
A viewer sees the panel but cannot use its greeting method. Asset loading and
JavaScript execution are separate from the broker's method authorization.

For publisher rotation, stage the new signed bundle with both key pairs as
explained above, then update the management entry to the newly approved key while
the host is stopped. When revoking an extension, remove its local entry and any
no-longer-needed grants; close open browser tabs because already loaded trusted
JavaScript cannot be recalled from them. Advance the authentication policy
generation when changing scopes or administrator principals.

## Component showcase

The `showcase.js` page consumes the versioned `sdk.ui` controls and displays
light and dark semantic theme samples, button and field states, keyboard tabs,
status and alert panels, a native dialog, and a responsive table. Page styles
use host semantic theme tokens. The showcase is an extension consumer and does
not replace or modify shared controls. To package it, copy only `showcase.js` into
a clean source directory and use the `amaterasu-web-extension pack` workflow
above with ID `showcase`, title `UI Elements`, entrypoint `showcase.js`, and no
method declarations. Stage and approve the signed extension through the normal
host workflow before opening it.
