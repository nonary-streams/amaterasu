#!/usr/bin/env python3
"""Real HTTP/authentication/catalog integration through the persistent native host."""
from __future__ import annotations

import http.client
import json
from pathlib import Path
import socket
import tempfile
import time
import xml.etree.ElementTree as ET

from test_component_updates import CARGO, run
from test_host_service import Console
from test_platform_modules import PlatformHarness, configure_steam_fixture

ORIGIN = "http://127.0.0.1:3000"
PASSWORD = "test-password-for-local-api-only"
PAIRING_FIXTURES = Path(__file__).resolve().parents[1] / "modules" / "pairing" / "tests" / "fixtures"


class Api:
    def __init__(self, console):
        status = console.request("amaterasu.management/1", {"action": "status"})
        assert status["ok"] and status["enabled"], status
        host, port = status["bound_address"].rsplit(":", 1)
        self.host, self.port = host, int(port)

    def post(self, path, payload=None, token=None, origin=None, extra=None):
        headers = {"Content-Type": "application/json"}
        if token is not None:
            headers["Authorization"] = "Bearer " + token
        if origin is not None:
            headers["Origin"] = origin
        headers.update(extra or {})
        connection = http.client.HTTPConnection(self.host, self.port, timeout=12)
        try:
            connection.request("POST", path, json.dumps(payload or {}), headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read()), dict(response.getheaders())
        finally:
            connection.close()

    def raw(self, headers):
        with socket.create_connection((self.host, self.port), timeout=12) as stream:
            stream.sendall(headers.encode())
            response = b""
            while data := stream.recv(4096):
                response += data
            return response


def test_auth_and_crud(console, api):
    setup = console.request("amaterasu.authentication/1", {
        "action": "setup", "username": "administrator", "password": PASSWORD,
    })
    assert setup["ok"], setup
    identity = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (PAIRING_FIXTURES / "host-cert.pem").read_text(),
        "private_key_pem": (PAIRING_FIXTURES / "host-key.pem").read_text(),
    })
    assert identity["ok"], identity
    health = api.raw(
        f"GET /healthz HTTP/1.1\r\nHost: {api.host}:{api.port}\r\nConnection: close\r\n\r\n"
    )
    head, body = health.split(b"\r\n\r\n", 1)
    assert head.startswith(b"HTTP/1.1 200"), health
    status = json.loads(body)
    assert status == {
        "ok": True,
        "authentication_verifier": True,
        "catalog_loaded": True,
        "pairing_identity": True,
    }, status
    deadline = time.monotonic() + 5
    nvhttp = None
    while time.monotonic() < deadline:
        candidate = console.request("amaterasu.nvhttp/1", {"action": "status"})
        if candidate.get("tls_ready"):
            nvhttp = candidate
            break
        time.sleep(0.025)
    assert nvhttp is not None, "NVHTTP never consumed the imported pairing identity"
    host, port = nvhttp["http_address"].rsplit(":", 1)
    connection = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        response = connection.getresponse()
        server_info = ET.fromstring(response.read())
    finally:
        connection.close()
    assert response.status == 200 and server_info.get("status_code") == "200"
    assert server_info.findtext("uniqueid"), "NVHTTP did not consume the pairing identity"
    status, failure, _ = api.post("/v1/catalog/list")
    assert status == 401 and not failure["ok"], failure
    status, failure, _ = api.post("/v1/auth/setup", {"username": "attacker", "password": PASSWORD})
    assert status == 404, failure
    status, failure, _ = api.post("/v1/auth/login", {"username": "administrator", "password": "wrong-password"})
    assert status == 401 and "wrong-password" not in json.dumps(failure), failure
    status, logged_in, headers = api.post("/v1/auth/login", {"username": "administrator", "password": PASSWORD}, origin=ORIGIN)
    assert status == 200 and logged_in["ok"], logged_in
    assert headers["Access-Control-Allow-Origin"] == ORIGIN
    administrator = logged_in["bearer"]
    status, failure, _ = api.post("/v1/catalog/list", token=administrator, origin="https://evil.invalid")
    assert status == 403 and administrator not in json.dumps(failure), failure
    status, catalog, _ = api.post("/v1/catalog/list", token=administrator)
    assert status == 200 and catalog["ok"], catalog
    entry = {"name": "HTTP Game", "launch": {"kind": "application", "path": "/Applications/Game.app"}}
    status, created, _ = api.post("/v1/catalog/create", {"entry": entry, "expected_revision": catalog["revision"]}, administrator)
    assert status == 200 and created["ok"], created
    status, found, _ = api.post("/v1/catalog/get", {"id": created["id"]}, administrator)
    assert status == 200 and found["entry"]["name"] == "HTTP Game", found
    status, renamed, _ = api.post("/v1/catalog/update", {"id": created["id"], "entry": dict(entry, name="Renamed"), "expected_revision": created["revision"]}, administrator)
    assert status == 200 and renamed["ok"], renamed
    status, deleted, _ = api.post("/v1/catalog/delete", {"id": created["id"], "expected_revision": renamed["revision"]}, administrator)
    assert status == 200 and deleted["ok"], deleted
    status, configured, _ = api.post("/v1/configuration/register", {"fields": {"fps": {"type": "integer", "default": 60}}}, administrator)
    assert status == 200 and configured["ok"], configured
    status, migrated, _ = api.post("/v1/configuration/migrate", {
        "expected_revision": configured["revision"], "fields": {"refresh": {"type": "integer", "default": 60}},
        "renames": {"fps": "refresh"}, "removed_fields": [], "values": {"refresh": 120},
    }, administrator)
    assert status == 200 and migrated["ok"] and migrated["values"]["refresh"] == 120, migrated
    status, host_settings, _ = api.post("/v1/nvhttp/settings", token=administrator)
    assert status == 200 and host_settings["hostname"] == "Amaterasu-test", host_settings
    status, default_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "", "expected_revision": host_settings["revision"],
    }, administrator)
    assert status == 200 and default_host["configured_hostname"] == "" and default_host["hostname"], default_host
    connection = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        default_info = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert default_info.findtext("hostname") == default_host["hostname"], ET.tostring(default_info)
    status, renamed_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games",
        "expected_revision": default_host["revision"],
    }, administrator)
    assert status == 200 and renamed_host["hostname"] == "Living Room & Games", renamed_host
    status, conflict, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "Stale", "expected_revision": host_settings["revision"],
    }, administrator)
    assert status == 200 and conflict["code"] == "revision_conflict", conflict
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Foreign", "expected_revision": renamed_host["revision"],
        "owner": "amaterasu.other",
    }, administrator)[0] == 400
    connection = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        refreshed = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert refreshed.findtext("hostname") == "Living Room & Games", ET.tostring(refreshed)
    status, steam_settings, _ = api.post("/v1/steam/settings", token=administrator)
    assert status == 200 and steam_settings["values"]["enabled"] is False, steam_settings
    status, blocked_refresh, _ = api.post("/v1/steam/refresh", token=administrator)
    assert status == 200 and blocked_refresh["ok"] is False, blocked_refresh
    status, steam_enabled, _ = api.post("/v1/steam/settings_update", {
        "enabled": True, "expected_revision": steam_settings["revision"],
    }, administrator)
    assert status == 200 and steam_enabled["values"]["enabled"] is True, steam_enabled
    status, stale, _ = api.post("/v1/steam/settings_update", {
        "enabled": False, "expected_revision": steam_settings["revision"],
    }, administrator)
    assert status == 200 and stale.get("code", stale.get("error", {}).get("code")) == "revision_conflict", stale
    status, refresh, _ = api.post("/v1/steam/refresh", token=administrator)
    assert status == 200 and refresh["ok"], refresh
    deadline = time.monotonic() + 5
    while True:
        status, games, _ = api.post("/v1/steam/list", token=administrator)
        assert status == 200 and games["ok"], games
        if games["complete"] and not games["scanning"]:
            break
        assert time.monotonic() < deadline, games
        time.sleep(0.02)
    assert len(games["games"]) == 1 and games["games"][0]["stable_id"] == "steam:730", games
    assert games["games"][0]["installed"], games
    token = console.request("amaterasu.authentication/1", {"action": "issue_token", "scopes": ["catalog.read"], "ttl_seconds": 3600})
    assert token["ok"], token
    status, result, _ = api.post("/v1/catalog/list", token=token["bearer"])
    assert status == 200 and result["ok"], result
    status, result, _ = api.post("/v1/catalog/create", {"entry": entry, "expected_revision": deleted["revision"]}, token["bearer"])
    assert status == 403 and not result["ok"], result
    assert api.post("/v1/nvhttp/settings", token=token["bearer"])[0] == 403
    assert api.post("/v1/steam/settings", token=token["bearer"])[0] == 403
    assert token["bearer"] not in json.dumps(result)
    # Route actions cannot be swapped through payloads or arbitrary command dispatch.
    assert api.post("/v1/catalog/list", {"action": "create"}, token["bearer"])[0] == 400
    assert api.post("/v1/runtime/request", {"service": "amaterasu.state/1"}, administrator)[0] == 404
    return token, administrator


def test_http_framing(api):
    authority = f"{api.host}:{api.port}"
    base = f"POST /v1/catalog/list HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n"
    for framing in ["Content-Length: 2\r\nContent-Length: 2\r\n", "Transfer-Encoding: chunked\r\nContent-Length: 2\r\n"]:
        assert api.raw(base + framing + "\r\n{}").startswith(b"HTTP/1.1 400")
    assert api.raw(base + "Content-Length: 12001\r\n\r\n").startswith(b"HTTP/1.1 413")
    preflight = f"OPTIONS /v1/catalog/create HTTP/1.1\r\nHost: {authority}\r\nOrigin: {ORIGIN}\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n\r\n"
    response = api.raw(preflight)
    assert response.startswith(b"HTTP/1.1 204") and b"Access-Control-Allow-Origin:" in response
    assert api.post("/v1/auth/login", extra={"Host": "evil.invalid"})[0] == 400


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-management-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure_steam_fixture(harness)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.management"] = {
            "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
        }
        bootstrap["policy"]["settings"]["amaterasu.nvhttp"] = {
            "enabled": True,
            "http_bind": "127.0.0.1:0",
            "https_bind": "127.0.0.1:0",
            "allow_remote": False,
            "hostname": "Amaterasu-test",
        }
        bootstrap_path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        console = Console(harness)
        try:
            api = Api(console)
            token, session = test_auth_and_crud(console, api)
            test_http_framing(api)
            console.eof()
        finally:
            console.close()
        console = Console(harness)
        try:
            api = Api(console)
            status, host_settings, _ = api.post("/v1/nvhttp/settings", token=token["bearer"])
            assert status == 403, host_settings
            # The administrator login session expires on worker restart; a new
            # login confirms product settings survived without leaking access.
            status, relogged, _ = api.post("/v1/auth/login", {"username": "administrator", "password": PASSWORD})
            assert status == 200 and relogged["ok"], relogged
            status, host_settings, _ = api.post("/v1/nvhttp/settings", token=relogged["bearer"])
            assert status == 200 and host_settings["hostname"] == "Living Room & Games", host_settings
            status, steam_settings, _ = api.post("/v1/steam/settings", token=relogged["bearer"])
            assert status == 200 and steam_settings["values"]["enabled"] is True, steam_settings
            assert api.post("/v1/catalog/list", token=token["bearer"])[0] == 200
            # Each read performs auth verification + state lookup and catalog +
            # state lookup: over 512 internal RPCs must not exhaust retained slots.
            for _ in range(160):
                status, payload, _ = api.post("/v1/catalog/list", token=token["bearer"])
                assert status == 200 and payload["ok"], payload
            assert api.post("/v1/catalog/list", token=session)[0] == 403
            revoked = console.request("amaterasu.authentication/1", {"action": "revoke_token", "token_id": token["token_id"]})
            assert revoked["ok"], revoked
            assert api.post("/v1/catalog/list", token=token["bearer"])[0] == 403
            console.eof()
        finally:
            console.close()
    print("Passed: real HTTP login/authentication, route scopes, catalog CRUD, token persistence/revocation,")
    print("Moonlight hostname/Steam settings CAS, discovery and restart persistence, session restart invalidation,")
    print("strict framing, Origin/Host checks, preflight and no secret-bearing errors.")


if __name__ == "__main__":
    main()
