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


def available_moonlight_port_pair():
    for _ in range(30):
        probes = []
        try:
            http_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probes.append(http_socket)
            http_socket.bind(("127.0.0.1", 0))
            port = http_socket.getsockname()[1]
            if not 1029 <= port <= 65514:
                continue
            for socket_type, offset in (
                (socket.SOCK_STREAM, -5),
                (socket.SOCK_STREAM, 21),
                (socket.SOCK_DGRAM, 9),
                (socket.SOCK_DGRAM, 10),
                (socket.SOCK_DGRAM, 11),
            ):
                probe = socket.socket(socket.AF_INET, socket_type)
                probes.append(probe)
                probe.bind(("127.0.0.1", port + offset))
            return port
        except OSError:
            continue
        finally:
            for probe in probes:
                probe.close()
    raise AssertionError("no available Moonlight/RTSP/media port set")


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
    status, auth_settings, _ = api.post("/v1/auth/settings", token=administrator)
    assert status == 200 and auth_settings["ok"], auth_settings
    assert auth_settings["values"] == {
        "session_token_ttl_seconds": 7200,
        "remember_me_refresh_token_ttl_seconds": 604800,
    }, auth_settings
    auth_values = {
        "session_token_ttl_seconds": 1800,
        "remember_me_refresh_token_ttl_seconds": 3600,
    }
    status, saved_auth_settings, _ = api.post(
        "/v1/auth/settings_update",
        {**auth_values, "expected_revision": auth_settings["revision"]},
        administrator,
    )
    assert status == 200 and saved_auth_settings["ok"], saved_auth_settings
    assert saved_auth_settings["values"] == auth_values, saved_auth_settings
    status, auth_conflict, _ = api.post(
        "/v1/auth/settings_update",
        {**auth_values, "expected_revision": auth_settings["revision"]},
        administrator,
    )
    assert status == 200 and auth_conflict["error"]["code"] == "revision_conflict", auth_conflict
    status, old_session_settings, _ = api.post("/v1/auth/settings", token=administrator)
    assert status == 200 and old_session_settings["values"] == auth_values, old_session_settings
    issued_at = int(time.time())
    status, new_login, _ = api.post(
        "/v1/auth/login",
        {"username": "administrator", "password": PASSWORD, "remember_me": True},
    )
    assert status == 200 and new_login["ok"], new_login
    assert 0 <= new_login["expires_at"] - issued_at <= 1801, new_login
    assert new_login["refresh_expires_at"] - new_login["expires_at"] == 1800, new_login
    assert api.post("/v1/catalog/list", token=administrator)[0] == 200
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
    assert host_settings["active_http_port"] == host_settings["port"], host_settings
    assert host_settings["active_https_port"] == host_settings["port"] - 5, host_settings
    status, default_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "", "port": host_settings["port"],
        "expected_revision": host_settings["revision"],
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
        "port": default_host["port"],
        "expected_revision": default_host["revision"],
    }, administrator)
    assert status == 200 and renamed_host["hostname"] == "Living Room & Games", renamed_host
    status, conflict, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "Stale", "port": renamed_host["port"],
        "expected_revision": host_settings["revision"],
    }, administrator)
    assert status == 200 and conflict["code"] == "revision_conflict", conflict
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Foreign", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "owner": "amaterasu.other",
    }, administrator)[0] == 400
    connection = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        refreshed = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert refreshed.findtext("hostname") == "Living Room & Games", ET.tostring(refreshed)
    moved_port = available_moonlight_port_pair()
    status, moved_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": moved_port,
        "expected_revision": renamed_host["revision"],
    }, administrator)
    assert status == 200 and moved_host["ok"], moved_host
    assert moved_host["active_http_port"] == moved_port, moved_host
    assert moved_host["active_https_port"] == moved_port - 5, moved_host
    connection = http.client.HTTPConnection(host, moved_port, timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        moved_info = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert moved_info.findtext("hostname") == "Living Room & Games", ET.tostring(moved_info)
    status, steam_settings, _ = api.post("/v1/steam/settings", token=administrator)
    assert status == 200 and steam_settings["ok"], steam_settings
    assert steam_settings["values"] == {
        "enabled": steam_settings["values"]["enabled"],
        "auto_sync": False,
        "exclude_games": [],
        "include_tools": False,
        "sync_all_installed": False,
        "recent_games": 10,
        "recent_max_age_days": 30,
    }, steam_settings
    if not steam_settings["values"]["enabled"]:
        status, blocked_refresh, _ = api.post("/v1/steam/refresh", token=administrator)
        assert status == 200 and blocked_refresh["ok"] is False, blocked_refresh
    steam_values = {
        "enabled": True,
        "auto_sync": True,
        "exclude_games": [
            {"id": "731", "name": "Future Game"},
            {"name": "Uninstalled Title"},
        ],
        "include_tools": False,
        "sync_all_installed": False,
        "recent_games": 25,
        "recent_max_age_days": 45,
    }
    status, steam_enabled, _ = api.post("/v1/steam/settings_update", {
        **steam_values, "expected_revision": steam_settings["revision"],
    }, administrator)
    assert status == 200 and steam_enabled["values"] == steam_values, steam_enabled

    # Older clients still send the original three editable values; newer values
    # must be preserved by the provider's patch-compatible update contract.
    legacy_values = {
        "enabled": steam_values["enabled"],
        "auto_sync": False,
        "exclude_games": steam_values["exclude_games"],
    }
    status, steam_legacy_update, _ = api.post("/v1/steam/settings_update", {
        **legacy_values, "expected_revision": steam_enabled["revision"],
    }, administrator)
    steam_values = {**steam_values, **legacy_values}
    assert status == 200 and steam_legacy_update["values"] == steam_values, steam_legacy_update
    status, stale, _ = api.post("/v1/steam/settings_update", {
        "enabled": False, "auto_sync": False, "exclude_games": [],
        "expected_revision": steam_settings["revision"],
    }, administrator)
    assert status == 200 and stale.get("code", stale.get("error", {}).get("code")) == "revision_conflict", stale
    assert api.post("/v1/steam/settings_update", {
        **steam_values, "expected_revision": steam_enabled["revision"],
        "owner": "amaterasu.other",
    }, administrator)[0] == 400
    assert api.post("/v1/steam/settings_update", {
        "enabled": True, "auto_sync": True, "exclude_games": [{}],
        "expected_revision": steam_enabled["revision"],
    }, administrator)[0] == 400
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
    return token, administrator, moved_port, steam_values, auth_values, saved_auth_settings["revision"]


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
        initial_port = available_moonlight_port_pair()
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.management"] = {
            "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
        }
        bootstrap["policy"]["settings"]["amaterasu.nvhttp"] = {
            "enabled": True,
            "http_bind": f"127.0.0.1:{initial_port}",
            "https_bind": f"127.0.0.1:{initial_port - 5}",
            "allow_remote": False,
            "hostname": "Amaterasu-test",
        }
        bootstrap["policy"]["settings"]["amaterasu.rtsp"] = {
            "enabled": True,
            "allow_remote": False,
            "bind": f"127.0.0.1:{initial_port + 21}",
            "allowed_principals": ["amaterasu.nvhttp"],
        }
        bootstrap_path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        console = Console(harness)
        try:
            api = Api(console)
            token, session, moved_port, steam_values, auth_values, auth_revision = test_auth_and_crud(console, api)
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
            status, auth_settings, _ = api.post("/v1/auth/settings", token=relogged["bearer"])
            assert status == 200 and auth_settings["values"] == auth_values, auth_settings
            assert auth_settings["revision"] == auth_revision, auth_settings
            status, host_settings, _ = api.post("/v1/nvhttp/settings", token=relogged["bearer"])
            assert status == 200 and host_settings["hostname"] == "Living Room & Games", host_settings
            assert host_settings["port"] == moved_port and host_settings["active_http_port"] == moved_port, host_settings
            status, steam_settings, _ = api.post("/v1/steam/settings", token=relogged["bearer"])
            assert status == 200 and steam_settings["values"] == steam_values, steam_settings
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
    print("Moonlight hostname/port live rebind and Steam settings CAS, discovery and restart persistence,")
    print("session restart invalidation,")
    print("strict framing, Origin/Host checks, preflight and no secret-bearing errors.")


if __name__ == "__main__":
    main()
