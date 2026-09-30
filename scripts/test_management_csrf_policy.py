#!/usr/bin/env python3
"""Signed native management CSRF exceptions, authentication, CAS and restart.

Listeners bind only loopback and ephemeral ports. These HTTP probes do not
qualify installed-host browsers or credentialed cross-origin browser access.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import socket
import sys
import tempfile

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import Api, stop_console
from test_management_origin_policy import TlsApi, issue, ready
from test_platform_modules import PlatformHarness, configure_steam_fixture

TRUSTED = "https://trusted.example:47990"
OTHER = "https://other.example:47990"
CSRF_ONLY = "https://csrf-only.example:47990"


def configure_tls(harness):
    bootstrap_path = harness.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    tls_directory = harness.root / "data/state/management-tls"
    tls_directory.mkdir(mode=0o700, parents=True)
    files = {}
    fixtures = WORKSPACE / "modules/management/tests/fixtures"
    for role, filename in [("cert", "browser-cert.pem"), ("key", "browser-key.pem")]:
        contents = (fixtures / filename).read_bytes()
        name = f"management-{role}-{hashlib.sha256(contents).hexdigest()}.pem"
        path = tls_directory / name
        path.write_bytes(contents)
        path.chmod(0o600)
        files[role] = "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + name
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    bootstrap["policy"]["settings"]["amaterasu.management"].update({
        "enabled": True, "bind": "127.0.0.1:0", "tls_bind": f"127.0.0.1:{port}",
        "tls_certificate_path": files["cert"], "tls_private_key_path": files["key"],
        "allowed_origins": [TRUSTED, OTHER],
    })
    bootstrap_path.write_text(json.dumps(bootstrap))


def check(api, token, revision, selection):
    status, result, _ = api.post("/v1/management_settings/get", token=token)
    assert status == 200 and result["ok"], (status, result)
    assert result["revision"] == revision, result
    assert result["values"]["csrf_allowed_origins"] == selection, result
    assert result["csrf_allowed_origins"] == (selection or []), result
    assert result["values"]["origin_web_ui_allowed"] is None, result
    assert result["origin_web_ui_allowed"] == "lan", result


def main():
    if "--skip-build" not in sys.argv[1:]:
        run(CARGO, "build", "--workspace", "--offline")
        run("npm", "--prefix", WORKSPACE / "frontend", "run", "build")
    with tempfile.TemporaryDirectory(prefix="amaterasu-csrf-policy-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure_steam_fixture(harness)
        configure_tls(harness)
        harness.stage(harness.package(1))
        web_package = harness.directory / "csrf-web"
        run(harness.publisher, "pack-web", harness.root, WORKSPACE / "frontend/dist", web_package, "1")
        run(harness.publisher, "stage-web", harness.root, web_package)
        console = Console(harness)
        try:
            assert ready(console)["csrf_allowed_origins"] == []
            assert console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "csrf-administrator",
                "password": "isolated-csrf-policy-password",
            })["ok"]
            administrator = issue(console, ["configuration.read", "configuration.write"])
            reader = issue(console, ["configuration.read"])
            api = TlsApi(console)
            same_origin = f"https://{api.host}:{api.port}"
            status, session, _ = api.post("/v1/auth/login", {
                "username":"csrf-administrator", "password":"isolated-csrf-policy-password",
                "remember_me":True,
            })
            assert status == 200 and session["ok"] and session["kind"] == "administrator_session"
            cookie = {"Origin": TRUSTED, "Cookie": "__Secure-amaterasu-access=" + session["bearer"]}
            check(api, reader, 0, None)
            assert api.post("/v1/management_settings/get", extra=cookie)[1]["code"] == "csrf_denied"
            assert api.post("/v1/management_settings/update", {"expected_revision":0,"csrf_allowed_origins":[TRUSTED]}, reader)[0] == 403

            revision = 0
            def save(selection, omitted=False):
                nonlocal revision
                payload = {"expected_revision": revision}
                if not omitted:
                    payload["csrf_allowed_origins"] = selection
                status, result, _ = api.post("/v1/management_settings/update", payload, administrator)
                assert status == 200 and result["ok"], (status, result)
                revision += 1
                check(api, reader, revision, selection)

            save([TRUSTED])
            assert api.post("/v1/management_settings/get", extra=cookie)[1]["ok"]
            assert api.post("/v1/auth/browser/session", extra=cookie)[1]["ok"]
            assert api.post("/v1/management_settings/get", extra={"Origin":TRUSTED})[0] == 401
            assert api.post("/v1/management_settings/get", extra={**cookie,"Authorization":"junk"})[0] == 401
            assert api.post("/v1/management_settings/get", extra={**cookie,"Host":"evil.invalid"})[0] == 400
            assert api.post("/v1/management_settings/get", extra={**cookie,"Origin":OTHER})[1]["code"] == "csrf_denied"
            assert api.post("/v1/management_settings/get", extra={"Cookie":cookie["Cookie"],"Referer":TRUSTED + "/page"})[1]["code"] == "csrf_denied"
            assert Api(console).post("/v1/management_settings/get", extra=cookie)[0] == 401
            assert api.post("/v1/auth/browser/refresh", extra=cookie)[0] == 401
            assert api.post("/v1/auth/browser/login", {"username":"csrf-administrator","password":"wrong-password"}, extra={"Origin":TRUSTED})[0] == 401
            assert api.post("/v1/management_settings/update", {"expected_revision":revision,"csrf_allowed_origins":[]}, extra={"Origin":TRUSTED,"Cookie":"__Secure-amaterasu-access=" + reader})[0] == 403

            # An exemption neither changes CORS nor bypasses durable CAS revision checks.
            save([CSRF_ONLY])
            assert api.post("/v1/management_settings/get", extra={**cookie,"Origin":CSRF_ONLY})[1]["code"] == "origin_denied"
            status, result, _ = api.post("/v1/management_settings/update", {"expected_revision":revision-1,"csrf_allowed_origins":[TRUSTED]}, administrator)
            assert status == 200 and result["code"] == "revision_conflict", result
            for selection in ["https://host", ["http://host"], ["https://host/path"], ["https://host", "https://host"], ["https://host"] * 17]:
                assert api.post("/v1/management_settings/update", {"expected_revision":revision,"csrf_allowed_origins":selection}, administrator)[0] == 400
            save([TRUSTED])
            save([TRUSTED], omitted=True)
            stop_console(console)
            console = Console(harness)
            assert ready(console)["revision"] == revision
            api = TlsApi(console)
            check(api, reader, revision, [TRUSTED])
            assert api.post("/v1/management_settings/get", extra=cookie)[1]["ok"]

            save([])
            assert api.post("/v1/management_settings/get", extra=cookie)[1]["code"] == "csrf_denied"
            save(None)
            stop_console(console)
            console = Console(harness)
            assert ready(console)["revision"] == revision
            api = TlsApi(console)
            check(api, reader, revision, None)
            assert api.post("/v1/management_settings/get", extra=cookie)[1]["code"] == "csrf_denied"
            assert api.post("/v1/management_settings/get", extra={**cookie,"Origin":same_origin,"X-Amaterasu-CSRF":"1"})[1]["ok"]
            status, javascript, _ = api.get("/app.js")
            assert status == 200 and b"Save CSRF origins" in javascript and b"csrf_allowed_origins" in javascript
        finally:
            console.close()
    print("Passed: signed native HTTPS CSRF policy CAS/restart/readback; exact exemption, CORS/Host/TLS, credentials and route scopes remain independent (loopback only; installed browsers unqualified).")


if __name__ == "__main__":
    main()
