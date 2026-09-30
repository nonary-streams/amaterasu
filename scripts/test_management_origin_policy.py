#!/usr/bin/env python3
"""Signed host management policy CAS, scoped authentication and restart persistence.

Every listener binds loopback. CIDR and remote admission edges are qualified by
pure management unit tests, not by opening a remote or wildcard listener here.
"""
from __future__ import annotations

import http.client
import hashlib
import json
from pathlib import Path
import ssl
import socket
import sys
import tempfile
import time

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import Api, stop_console
from test_platform_modules import PlatformHarness, configure_steam_fixture

SERVICE = "amaterasu.management/1"
FIXTURES = WORKSPACE / "modules/management/tests/fixtures"


class TlsApi:
    def __init__(self, console):
        status = console.request(SERVICE, {"action": "status"})
        self.host, port = status["tls_address"].rsplit(":", 1)
        self.port = int(port)
        self.context = ssl.create_default_context(cafile=str(FIXTURES / "browser-cert.pem"))

    def post(self, path, payload=None, token=None, extra=None):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        headers.update(extra or {})
        connection = http.client.HTTPSConnection(self.host, self.port, context=self.context, timeout=12)
        try:
            connection.request("POST", path, json.dumps(payload or {}), headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read()), dict(response.getheaders())
        finally:
            connection.close()

    def get(self, path):
        connection = http.client.HTTPSConnection(self.host, self.port, context=self.context, timeout=12)
        try:
            connection.request("GET", path)
            response = connection.getresponse()
            return response.status, response.read(), dict(response.getheaders())
        finally:
            connection.close()


def ready(console):
    for _ in range(100):
        result = console.request(SERVICE, {"action": "product_get"})
        if result.get("ok"):
            return result
        assert result.get("code") == "settings_unavailable", result
        time.sleep(0.025)
    raise AssertionError("management durable policy did not load")


def issue(console, scopes):
    result = console.request("amaterasu.authentication/1", {
        "action": "issue_token", "scopes": scopes, "ttl_seconds": 3600,
    })
    assert result["ok"], result
    return result["bearer"]


def check(api, expected_revision, selection, token):
    status, result, _ = api.post("/v1/management_settings/get", token=token)
    assert status == 200 and result["ok"], result
    assert result["revision"] == expected_revision, result
    assert result["values"] == {"origin_web_ui_allowed": selection,"csrf_allowed_origins":None}, result
    assert result["origin_web_ui_allowed"] == (selection or "lan"), result
    return result


def main():
    if "--skip-build" not in sys.argv[1:]:
        run(CARGO, "build", "--workspace", "--offline")
        run("npm", "--prefix", WORKSPACE / "frontend", "run", "build")
    with tempfile.TemporaryDirectory(prefix="amaterasu-management-policy-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure_steam_fixture(harness)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        tls_files = {}
        state_root = harness.root / "data/state"
        state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
        tls_directory = state_root / "management-tls"
        tls_directory.mkdir(mode=0o700)
        for role, source in [("cert", "browser-cert.pem"), ("key", "browser-key.pem")]:
            contents = (FIXTURES / source).read_bytes()
            name = f"management-{role}-{hashlib.sha256(contents).hexdigest()}.pem"
            target = tls_directory / name
            target.write_bytes(contents)
            target.chmod(0o600)
            tls_files[role] = "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + name
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.bind(("127.0.0.1", 0))
            tls_port = probe.getsockname()[1]
        bootstrap["policy"]["settings"]["amaterasu.management"].update({
            "enabled": True, "bind": "127.0.0.1:0", "tls_bind": f"127.0.0.1:{tls_port}",
            "tls_certificate_path": tls_files["cert"],
            "tls_private_key_path": tls_files["key"],
        })
        bootstrap_path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        web_package = harness.directory / "policy-web"
        run(harness.publisher, "pack-web", harness.root, WORKSPACE / "frontend/dist", web_package, "1")
        run(harness.publisher, "stage-web", harness.root, web_package)
        console = Console(harness)
        try:
            initial = ready(console)
            assert initial["revision"] == 0 and initial["origin_web_ui_allowed"] == "lan", initial
            setup = console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "policy-administrator", "password": "isolated-policy-test-password",
            })
            assert setup["ok"], setup
            administrator = issue(console, ["configuration.read", "configuration.write"])
            reader = issue(console, ["configuration.read"])
            unrelated = issue(console, ["catalog.read"])
            api = TlsApi(console)
            plain = Api(console)
            status, javascript, headers = api.get("/app.js")
            assert status == 200 and b"management_settings/update" in javascript and b"Management access" in javascript, (status, javascript[:400])
            assert headers["X-Content-Type-Options"] == "nosniff", headers
            assert api.post("/v1/management_settings/get")[0] == 401
            assert api.post("/v1/management_settings/get", token=unrelated)[0] == 403
            assert api.post("/v1/management_settings/update", {"expected_revision":0,"origin_web_ui_allowed":"wan"}, reader)[0] == 403
            assert api.post("/v1/management_settings/get", token=administrator, extra={"Origin":"https://untrusted.invalid"})[0] == 403
            assert api.post("/v1/management_settings/get", token=administrator, extra={"Host":"untrusted.invalid"})[0] == 400
            assert plain.post("/v1/auth/browser/session")[0] == 404
            check(api, 0, None, reader)

            # The generic user schema shares the management principal, but not its product namespace.
            status, registered, _ = api.post("/v1/configuration/register", {
                "fields":{"policy_user_value":{"type":"integer","default":17}},
            }, administrator)
            assert status == 200 and registered["ok"], registered
            revision = 0
            for selection in ["pc", "lan", "wan", None]:
                status, changed, _ = api.post("/v1/management_settings/update", {
                    "expected_revision":revision, "origin_web_ui_allowed":selection,
                }, administrator)
                assert status == 200 and changed["ok"], changed
                revision += 1
                check(api, revision, selection, reader)
                check(plain, revision, selection, reader)
            status, stale, _ = api.post("/v1/management_settings/update", {
                "expected_revision":revision - 1, "origin_web_ui_allowed":"wan",
            }, administrator)
            assert status == 200 and stale["code"] == "revision_conflict", stale
            for fields in [
                {"origin_web_ui_allowed":"WAN"}, {"origin_web_ui_allowed":False},
                {"origin_web_ui_allowed":"wan","caller":"other"},
                {"values":{"origin_web_ui_allowed":"wan"}},
            ]:
                assert api.post("/v1/management_settings/update", {"expected_revision":revision, **fields}, administrator)[0] == 400
            check(api, revision, None, reader)
            assert api.post("/v1/configuration/get", token=administrator)[1]["values"] == {"policy_user_value":17}

            # Omission retains the selected value while still advancing the CAS document.
            changed = api.post("/v1/management_settings/update", {"expected_revision":revision}, administrator)[1]
            assert changed["ok"], changed
            revision += 1
            check(api, revision, None, reader)
            assert console.request("amaterasu.configuration/1", {"action":"product_get"})["found"] is False
            stop_console(console)
            console = Console(harness)
            restored = ready(console)
            assert restored["revision"] == revision and restored["values"]["origin_web_ui_allowed"] is None, restored
            check(TlsApi(console), revision, None, reader)
            assert Api(console).post("/v1/configuration/get", token=administrator)[1]["values"] == {"policy_user_value":17}
        finally:
            console.close()
    print("Passed: signed editor delivery over verified TLS, management policy scope/Host/Origin checks, pc/lan/wan/null and omission CAS, generic schema isolation, token authority and restart persistence (loopback listeners only).")


if __name__ == "__main__":
    main()
