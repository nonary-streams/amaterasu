#!/usr/bin/env python3
"""Pairing and display permissions through real loaded libraries and HTTP."""
from __future__ import annotations

import json
from pathlib import Path
import tempfile

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import Api
from test_platform_modules import PlatformHarness


def token(console, scopes):
    result = console.request("amaterasu.authentication/1", {
        "action": "issue_token", "scopes": scopes, "ttl_seconds": 3600,
    })
    assert result["ok"], result
    return result["bearer"]


def test_pairing(console, api):
    fixtures = WORKSPACE / "modules/pairing/tests/fixtures"
    identity = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (fixtures / "host-cert.pem").read_text(),
        "private_key_pem": (fixtures / "host-key.pem").read_text(),
    })
    assert identity["ok"], identity
    administrator = token(console, ["pairing.admin"])
    unauthorized = token(console, ["catalog.read"])
    assert api.post("/v1/pairing/identity", token=unauthorized)[0] == 403
    status, public, _ = api.post("/v1/pairing/identity", token=administrator)
    assert status == 200 and public["certificate_id"] == identity["certificate_id"], public
    assert "PRIVATE KEY" not in json.dumps(public)
    pending = console.request("amaterasu.pairing/1", {
        "action": "begin", "client_id": "http-test-client", "name": "Pairing test",
        "client_certificate_pem": (fixtures / "client-cert.pem").read_text(),
        "salt": "01" * 16,
    })
    assert pending["ok"], pending
    status, listed, _ = api.post("/v1/pairing/pending", token=administrator)
    assert status == 200 and pending["pairing_id"] in json.dumps(listed), listed
    status, approved, _ = api.post("/v1/pairing/approve_pin", {
        "pairing_id": pending["pairing_id"], "pin": "1234",
    }, administrator)
    assert status == 200 and approved["ok"], approved
    assert "1234" not in json.dumps(approved)
    assert console.request("amaterasu.pairing/1", {
        "action": "get_server_cert", "pairing_id": pending["pairing_id"],
    })["paired"] == 1
    for action in ["initialize", "import_identity", "import_client", "begin", "check_client_certificate"]:
        assert api.post("/v1/pairing/" + action, token=administrator)[0] == 404
    assert api.post("/v1/pairing/identity", {"action": "import_identity"}, administrator)[0] == 400
    assert console.request("amaterasu.pairing/1", {"action": "cancel", "pairing_id": pending["pairing_id"]})["ok"]
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_client", "client_id": "imported-test", "name": "Imported test",
        "certificate_pem": (fixtures / "client-cert.pem").read_text(),
    })
    assert imported["ok"], imported
    status, clients, _ = api.post("/v1/pairing/list_clients", token=administrator)
    assert status == 200 and len(clients["clients"]) == 1, clients
    certificate_id = clients["clients"][0]["certificate_id"]
    assert api.post("/v1/pairing/unpair", {"certificate_id": certificate_id}, unauthorized)[0] == 403
    status, removed, _ = api.post("/v1/pairing/unpair", {"certificate_id": certificate_id}, administrator)
    assert status == 200 and removed["ok"], removed
    assert api.post("/v1/pairing/list_clients", token=administrator)[1]["clients"] == []


def test_display(console, api):
    reader = token(console, ["display.read"])
    writer = token(console, ["display.write"])
    status, state, _ = api.post("/v1/display/status", token=reader)
    assert status == 200 and state["ok"] and not state["mutation_enabled"], state
    assert state["state"] == "idle" and state["lease"] is None, state
    assert api.post("/v1/display/restore", {"lease": "unknown"}, reader)[0] == 403
    status, refused, _ = api.post("/v1/display/restore", {"lease": "unknown"}, writer)
    assert status == 200 and not refused["ok"], refused
    assert api.post("/v1/display/status", {"owner": "other"}, reader)[0] == 400


def test_media_discovery(console, api):
    reader = token(console, ["media.read"])
    unauthorized = token(console, ["catalog.read"])
    for route in ["dxgi_outputs", "wgc_outputs", "audio_endpoints", "encoders", "audio_encoders", "video_status"]:
        path = "/v1/media/" + route
        assert api.post(path, token=unauthorized)[0] == 403
        status, result, _ = api.post(path, token=reader)
        assert status == 200 and isinstance(result.get("ok"), bool), result
        if route == "audio_endpoints":
            assert result["ok"] and result["endpoints"] == [] and not result["enabled"], result
        elif route in ("encoders", "audio_encoders"):
            assert result["ok"] and result["encoders"] == [] and not result["enabled"], result
        elif route == "video_status":
            assert result["ok"] and result["streams"] == [] and not result["enabled"], result
        assert api.post(path, {"service": "amaterasu.audio/1"}, reader)[0] == 400
        assert api.post(path, {"factory": 1}, reader)[0] == 400
    for action in ["open", "close", "native_bind", "native_register", "read_audio"]:
        assert api.post("/v1/media/" + action, token=reader)[0] == 404


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-domains-") as temporary:
        harness = PlatformHarness(Path(temporary))
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.management"].update(enabled=True, bind="127.0.0.1:0")
        path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        console = Console(harness)
        try:
            assert console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "administrator", "password": "local-test-password-only",
            })["ok"]
            api = Api(console)
            test_pairing(console, api)
            test_display(console, api)
            test_media_discovery(console, api)
            console.eof()
        finally:
            console.close()
    print("Passed: pairing PIN approval, public identity, forbidden transport/key routes, display scopes, and scoped media discovery over real HTTP.")


if __name__ == "__main__":
    main()
