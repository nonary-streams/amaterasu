#!/usr/bin/env python3
"""Opt-in real Chromium qualification for signed and reusable plugin UI controls.

Runs a private loopback host, packages signed test extensions, the showcase,
and a zero-method UI primitives consumer, then drives owned Chromium over its
loopback-only CDP endpoint. UI primitive calls need no mocked service. The
existing generic settings-controls check uses an in-page API stub. It changes no
installed host, shared repository, firewall rule, or non-loopback listener.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from cryptography import x509
from cryptography.hazmat.primitives import serialization

from test_component_updates import CARGO, WORKSPACE, run
import test_management_private_network as private
from test_management_api import stop_console
from test_network_accounts_extensions import PASSWORD, test_enrollment_and_tls
from test_platform_modules import PlatformHarness
from test_signed_browser_session import OwnedBrowser

EXTENSION_ID = "plugin-ui-qualification"
SHOWCASE_ID = "showcase"
PRIMITIVES_ID = "signed-ui-primitives"
SHOWCASE_SOURCE = WORKSPACE / "examples/web-extension"
SHOWCASE_ASSETS = ("showcase.js", "showcase.css")
THEME_ASSETS = ("okami-botanical.svg", "okami-mountains.svg")
METHODS = WORKSPACE / "examples/web-extension/methods.json"
FIXTURE = WORKSPACE / "frontend/tests/fixtures/plugin-ui-browser/example.js"
PRIMITIVES_FIXTURE = WORKSPACE / "frontend/tests/fixtures/plugin-ui-browser/primitives.js"
BROWSER_DRIVER = WORKSPACE / "frontend/tests/signed-plugin-ui-browser-qualification.mjs"


def configure_signed_extension(harness: PlatformHarness) -> tuple[str, dict[str, str], str]:
    source = harness.directory / "plugin-ui-extension-source"
    source.mkdir()
    shutil.copy2(FIXTURE, source / "example.js")

    secret = harness.directory / "plugin-ui-publisher-key"
    secret.write_text("42" * 32)
    secret.chmod(0o600)
    methods = json.loads(METHODS.read_text())
    methods_path = harness.directory / "plugin-ui-methods.json"
    methods_path.write_text(json.dumps(methods))

    binary = harness.artifacts / "amaterasu-web-extension"
    bundle = harness.directory / "plugin-ui-extension-bundle"
    packed = run(binary, "pack", source, bundle, EXTENSION_ID, "Plugin UI Qualification",
                 "example.js", "1", str(int(time.time()) + 3600), secret, methods_path)
    values = dict(line.split(": ", 1) for line in packed.splitlines() if ": " in line)
    fingerprint = values["publisher fingerprint"]
    public_key = values["publisher public key"]

    staged_root = harness.directory / EXTENSION_ID
    run(binary, "stage", bundle, staged_root, fingerprint, public_key)

    showcase_source = harness.directory / "showcase-source"
    showcase_source.mkdir()
    for name in SHOWCASE_ASSETS:
        shutil.copy2(SHOWCASE_SOURCE / name, showcase_source / name)
    showcase_bundle = harness.directory / "showcase-bundle"
    run(binary, "pack", showcase_source, showcase_bundle, SHOWCASE_ID, "UI Elements",
        "showcase.js", "1", str(int(time.time()) + 3600), secret)
    showcase_root = harness.directory / SHOWCASE_ID
    run(binary, "stage", showcase_bundle, showcase_root, fingerprint, public_key)

    primitives_source = harness.directory / "signed-ui-primitives-source"
    primitives_source.mkdir()
    shutil.copy2(PRIMITIVES_FIXTURE, primitives_source / "primitives.js")
    primitives_bundle = harness.directory / "signed-ui-primitives-bundle"
    run(binary, "pack", primitives_source, primitives_bundle, PRIMITIVES_ID,
        "Signed UI Primitives Qualification", "primitives.js", "1",
        str(int(time.time()) + 3600), secret)
    primitives_root = harness.directory / PRIMITIVES_ID
    run(binary, "stage", primitives_bundle, primitives_root, fingerprint, public_key)

    bootstrap_path = harness.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.management"]["extensions"] = {
        "entries": [
            {
                "id": EXTENSION_ID,
                "root": str(staged_root),
                "trusted_keys": {fingerprint: public_key},
                "methods": methods,
            },
            {
                "id": SHOWCASE_ID,
                "root": str(showcase_root),
                "trusted_keys": {fingerprint: public_key},
                "methods": [],
            },
            {
                "id": PRIMITIVES_ID,
                "root": str(primitives_root),
                "trusted_keys": {fingerprint: public_key},
                "methods": [],
            },
        ],
    }
    policy = bootstrap["policy"]
    authentication = policy["settings"]["amaterasu.authentication"]
    if "account.self" not in authentication["allowed_scopes"]:
        authentication["allowed_scopes"].append("account.self")
        authentication["authorization_policy_generation"] += 1
    policy["principals"]["example.extension-provider"] = {"provide": ["example.extension/1"]}
    policy["principals"]["amaterasu.management"].setdefault("call", []).append("example.extension/1")
    policy.setdefault("api_methods", []).append({
        "provider": "example.extension-provider",
        "callers": ["amaterasu.management"],
        "method": {
            "service": "example.extension/1",
            "method": "greet",
            "interface_major": 1,
            "interface_minor": 0,
            "request": {"required": {"name": "string"}, "optional": {}, "allow_extra": False},
            "response": {"required": {"message": "string"}, "optional": {}, "allow_extra": False},
            "privilege": "user",
        },
    })
    assert any(
        approval["provider"] == "amaterasu.frame-limiter"
        and approval["method"]["service"] == "nonary.frame-limiter/1"
        and approval["method"]["method"] == "settings_validate"
        for approval in policy["api_methods"]
    ), "signed extension setup must preserve the default frame-limiter API approval"
    bootstrap_path.write_text(json.dumps(bootstrap))
    return (
        hashlib.sha256(FIXTURE.read_bytes()).hexdigest(),
        {
            name: hashlib.sha256((SHOWCASE_SOURCE / name).read_bytes()).hexdigest()
            for name in SHOWCASE_ASSETS
        },
        hashlib.sha256(PRIMITIVES_FIXTURE.read_bytes()).hexdigest(),
    )


def enrollment_code(harness: PlatformHarness) -> str:
    root = harness.root
    if not root.is_absolute() or root.resolve() != root or root.stat().st_mode & 0o077:
        raise AssertionError("private host root is not an absolute private directory")
    command = {"op": "request", "service": "amaterasu.authentication/1",
               "payload": {"action": "generate_enrollment_code"},
               "timeout_ms": 2000, "operation_id": "plugin-ui.enrollment"}
    result = subprocess.run(
        [str(harness.host), "probe", str(root), json.dumps(command)],
        capture_output=True, text=True, timeout=90,
    )
    if result.returncode != 0:
        raise AssertionError("private host candidate/enrollment probe failed: " + result.stderr[-3000:])
    try:
        reply = json.loads(result.stdout)
        payload = reply.get("payload", {})
    except json.JSONDecodeError as error:
        raise AssertionError("private host returned malformed enrollment data") from error
    code = payload.get("code")
    if reply.get("kind") != "result" or payload.get("ok") is not True or not isinstance(code, str):
        raise AssertionError("private host returned no enrollment code")
    return code


def spki_pin(certificate_path: Path) -> str:
    certificate = x509.load_pem_x509_certificate(certificate_path.read_bytes())
    public = certificate.public_key().public_bytes(
        serialization.Encoding.DER,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    return base64.b64encode(hashlib.sha256(public).digest()).decode("ascii")


def run_browser(
    harness: PlatformHarness,
    api,
    chromium: str,
    asset_digest: str,
    showcase_digests: dict[str, str],
    primitives_digest: str,
    certificate,
) -> None:
    browser = OwnedBrowser(harness.directory / "plugin-ui-browser", chromium, spki_pin(certificate))
    try:
        debugger_url = browser.start()
        payload = {
            "debuggerUrl": debugger_url,
            "origin": f"https://{api.host}:{api.port}",
            "profile": str(browser.profile),
            "spki": browser.spki,
            "username": "admin",
            "password": PASSWORD,
            "assetPath": str(FIXTURE),
            "assetSha256": asset_digest,
            "primitivesPath": str(PRIMITIVES_FIXTURE),
            "primitivesSha256": primitives_digest,
            "showcaseAssets": [
                {
                    "name": name,
                    "path": str(SHOWCASE_SOURCE / name),
                    "sha256": digest,
                }
                for name, digest in showcase_digests.items()
            ],
            "themeAssets": [
                {
                    "name": name,
                    "path": str(WORKSPACE / "frontend/dist" / name),
                    "sha256": hashlib.sha256(
                        (WORKSPACE / "frontend/dist" / name).read_bytes()
                    ).hexdigest(),
                }
                for name in THEME_ASSETS
            ],
        }
        result = subprocess.run(
            ["node", str(BROWSER_DRIVER)],
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode != 0:
            raise AssertionError("browser qualification failed: " + result.stderr[-3000:])
        print(result.stdout.strip())
    finally:
        browser.close()


def build_workspace() -> None:
    result = subprocess.run(
        [CARGO, "build", "--workspace", "--offline"],
        cwd=WORKSPACE,
        capture_output=True,
        text=True,
        timeout=900,
    )
    if result.returncode:
        raise AssertionError(
            "workspace build failed: " + result.stdout[-3000:] + result.stderr[-5000:]
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true",
                        help="use existing workspace binaries and frontend/dist")
    parser.add_argument("--stage-only", action="store_true",
                        help="pack and verify the signed fixture without starting a host/browser")
    args = parser.parse_args()

    chromium = None
    if not args.stage_only:
        chromium = shutil.which("chromium") or shutil.which("chromium-browser")
        if not chromium:
            raise SystemExit("Chromium is required; install chromium or chromium-browser and retry")

    if not args.skip_build:
        build_workspace()
        npm = shutil.which("npm")
        if not npm:
            raise SystemExit("npm is required to build frontend assets")
        run(npm, "--prefix", WORKSPACE / "frontend", "ci", "--offline")
        run(npm, "--prefix", WORKSPACE / "frontend", "run", "build")

    with tempfile.TemporaryDirectory(prefix="amaterasu-plugin-ui-browser-") as temporary:
        harness = PlatformHarness(Path(temporary))
        xdg_data = harness.directory / "xdg-data"
        xdg_data.mkdir(mode=0o700)
        os.environ["XDG_DATA_HOME"] = str(xdg_data)
        xdg_runtime = harness.directory / "xdg-runtime"
        xdg_runtime.mkdir(mode=0o700)
        os.environ["XDG_RUNTIME_DIR"] = str(xdg_runtime)
        private.SERVER_ADDRESS = "127.0.0.1"
        certificate = private.configure_host(harness)
        asset_digest, showcase_digests, primitives_digest = configure_signed_extension(harness)
        if args.stage_only:
            print("PASS: signed test extension, showcase assets, and zero-method UI primitives consumer staged; browser delivery was not run")
            return

        package = harness.directory / "plugin-ui-platform"
        run(harness.publisher, "pack-extension-example", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        web_package = harness.directory / "plugin-ui-platform-web"
        run(harness.publisher, "pack-web", harness.root, WORKSPACE / "frontend/dist",
            web_package, "1")
        run(harness.publisher, "stage-web", harness.root, web_package)

        code = enrollment_code(harness)
        console = private.start_ready_console(harness)
        try:
            private.ready(console)
            api = private.PeerTlsApi(console, certificate, private.SERVER_ADDRESS)
            administrator = test_enrollment_and_tls(api, code)
            status, listing = api.request("POST", "/v1/extensions/list", {},
                                          token=administrator["bearer"])
            assert status == 200 and any(
                entry.get("id") == EXTENSION_ID for entry in listing.get("extensions", [])
            ), ("signed test extension is not discoverable through the authenticated host API "
                f"(HTTP {status}, response keys {sorted(listing) if isinstance(listing, dict) else type(listing).__name__}, "
                f"extension IDs {[entry.get('id') for entry in listing.get('extensions', [])] if isinstance(listing, dict) else []})")
            assert any(
                entry.get("id") == SHOWCASE_ID for entry in listing.get("extensions", [])
            ), "signed showcase is not discoverable through the authenticated host API"
            assert any(
                entry.get("id") == PRIMITIVES_ID for entry in listing.get("extensions", [])
            ), "signed UI primitives fixture is not discoverable through the authenticated host API"
            run_browser(harness, api, chromium, asset_digest, showcase_digests,
                        primitives_digest, certificate)
        finally:
            stop_console(console)


if __name__ == "__main__":
    main()
