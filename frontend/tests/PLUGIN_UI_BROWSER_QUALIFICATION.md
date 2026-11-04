# Plugin UI browser qualification

These opt-in checks do not modify shared UI implementation files.

## Reusable controls in Chromium

Install the frontend dependency from the lockfile, then run:

```sh
npm --prefix frontend ci
node frontend/tests/plugin-controls-browser-qualification.mjs
```

This launches an owned headless Chromium with a temporary profile and a local
simulated settings API. It checks keyboard adjustment and submit, accessible
labels, locale changes, preservation of provider-owned labels, and stale-view
disposal. It does not test a native provider or persistence.

## Signed web extension

On Linux, with the workspace toolchain and Python `cryptography` available:

```sh
python3 scripts/test_plugin_ui_browser_qualification.py --stage-only
python3 scripts/test_plugin_ui_browser_qualification.py
```

The first command creates a private host root, packages and stages a signed web
extension, and exits without claiming browser delivery. The full command also
builds the frontend, starts the isolated signed host and owned Chromium, then
checks signed asset HTTP delivery and bytes, keyboard interaction, a host API
call, locale changes, navigation cleanup, and remounting. It uses only loopback
addresses and temporary files. `--skip-build` uses existing workspace binaries
and `frontend/dist`.

The browser check does not qualify settings persistence: its reusable settings
control API is intentionally stubbed. The signed asset and extension method call
use the real private host. A full run requires the extension-example package to
activate successfully; it fails before browser startup if native package
activation fails.
