#!/usr/bin/env python3
"""Real HTTP/authentication/catalog integration through the persistent native host."""
from __future__ import annotations

import base64
import http.client
import json
import os
from pathlib import Path
import random
import socket
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

from test_component_updates import CARGO, run
from test_host_service import Console
from test_platform_modules import PlatformHarness, configure_steam_fixture

ORIGIN = "http://127.0.0.1:3000"
PASSWORD = "test-password-for-local-api-only"
PAIRING_FIXTURES = Path(__file__).resolve().parents[1] / "modules" / "pairing" / "tests" / "fixtures"
STEAM_ARTWORK = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII="
)


def available_moonlight_port_pair():
    # Windows can assign TCP ephemeral ports inside UDP exclusion ranges. Probe
    # non-ephemeral candidates as a fallback so this integration test can find
    # the entire Moonlight port set without relying on the TCP allocator.
    for candidate in [0] * 30 + random.sample(range(10000, 45000), 100):
        probes = []
        try:
            http_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probes.append(http_socket)
            http_socket.bind(("127.0.0.1", candidate))
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


def test_provider_override_route(api, administrator, fallback_id):
    cursor = None
    provider_row = None
    revision = None
    for _ in range(320):
        request = {"limit": 32}
        if cursor is not None:
            request["cursor"] = cursor
        status, page, _ = api.post("/v1/catalog/list", request, administrator)
        assert status == 200 and page["ok"], page
        revision = page["revision"]
        provider_row = next(
            (
                row
                for row in page["entries"]
                if row.get("entry", {}).get("launch", {}).get("kind") == "playnite"
            ),
            None,
        )
        if provider_row is not None or page.get("next_cursor") is None:
            break
        cursor = page["next_cursor"]
    else:
        raise AssertionError("catalog paging exceeded its bound while looking for Playnite entries")

    if provider_row is None:
        status, result, _ = api.post(
            "/v1/catalog/update_provider_overrides",
            {
                "id": fallback_id,
                "expected_revision": revision,
                "config_overrides": {"fec_percentage": 53},
            },
            administrator,
        )
        assert status == 200 and result.get("error", {}).get("code") in {
            "provider_snapshot_incomplete",
            "provider_entry_not_found",
        }, result
        print(
            "Skipped successful provider-owned override CAS/replay exercise: no finished Playnite snapshot was imported; the manual-row request was rejected."
        )
        return None, None, revision

    provider_id = provider_row["id"]
    original_override = provider_row["entry"].get("config_overrides", {}).get("fec_percentage")
    desired = 53 if original_override != 53 else 54
    status, saved, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": provider_id,
            "expected_revision": revision,
            "config_overrides": {"fec_percentage": desired},
        },
        administrator,
    )
    assert status == 200 and saved.get("ok") and saved.get("changed"), saved
    assert saved["config_overrides"] == {"fec_percentage": desired}, saved

    status, conflict, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": provider_id,
            "expected_revision": revision,
            "config_overrides": {"fec_percentage": desired + 1},
        },
        administrator,
    )
    assert status == 200 and conflict.get("error", {}).get("code") == "revision_conflict", conflict
    assert conflict["error"]["current_revision"] == saved["revision"], conflict

    status, replayed, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": provider_id,
            "expected_revision": revision,
            "config_overrides": {"fec_percentage": desired},
        },
        administrator,
    )
    assert status == 200 and replayed.get("ok") and replayed.get("replayed"), replayed
    assert replayed["revision"] == saved["revision"], replayed

    status, cleared, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": provider_id,
            "expected_revision": replayed["revision"],
            "config_overrides": {"fec_percentage": None},
        },
        administrator,
    )
    assert status == 200 and cleared.get("ok") and cleared.get("changed"), cleared
    assert cleared["config_overrides"] == {}, cleared
    status, restored, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": provider_id,
            "expected_revision": cleared["revision"],
            "config_overrides": {"fec_percentage": desired},
        },
        administrator,
    )
    assert status == 200 and restored.get("ok") and restored.get("changed"), restored
    status, readback, _ = api.post("/v1/catalog/get", {"id": provider_id}, administrator)
    assert status == 200 and readback["entry"].get("config_overrides", {}).get("fec_percentage") == desired, readback
    return provider_id, desired, restored["revision"]


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
    imported_clients = []
    for client_name, certificate_name in [
        ("Duplicate ID client one", "client-cert.pem"),
        ("Duplicate ID client two", "host-cert.pem"),
    ]:
        imported = console.request("amaterasu.pairing/1", {
            "action": "import_client",
            "client_id": "duplicate-client-id",
            "name": client_name,
            "certificate_pem": (PAIRING_FIXTURES / certificate_name).read_text(),
        })
        assert imported["ok"], imported
        imported_clients.append(client_name)
    status, paired_clients, _ = api.post("/v1/pairing/list_clients", token=administrator)
    assert status == 200 and paired_clients["ok"], paired_clients
    duplicates = [
        client for client in paired_clients["clients"]
        if client["client_id"] == "duplicate-client-id"
    ]
    assert len(duplicates) == 2, paired_clients
    first_client = next(client for client in duplicates if client["name"] == imported_clients[0])
    second_client = next(client for client in duplicates if client["name"] == imported_clients[1])
    status, forged_owner, _ = api.post(
        "/v1/pairing/update_client_config_overrides",
        {
            "certificate_id": first_client["certificate_id"],
            "client_id": second_client["client_id"],
            "expected_revision": paired_clients["revision"],
            "config_overrides": {"fec_percentage": 41},
        },
        administrator,
    )
    assert status == 400 and not forged_owner["ok"], forged_owner
    status, fec_update, _ = api.post(
        "/v1/pairing/update_client_config_overrides",
        {
            "certificate_id": first_client["certificate_id"],
            "expected_revision": paired_clients["revision"],
            "config_overrides": {"fec_percentage": 47, "stream_audio": True, "audio_sink": "client-endpoint", "virtual_sink": "client-virtual-endpoint", "audio_sink_capture_only": False, "back_button_timeout": -1, "controller": False, "gamepad": "auto", "keyboard": False, "mouse": True},
        },
        administrator,
    )
    assert status == 200 and fec_update["ok"], fec_update
    status, paired_after_update, _ = api.post("/v1/pairing/list_clients", token=administrator)
    assert status == 200 and paired_after_update["ok"], paired_after_update
    first_after_update = next(
        client for client in paired_after_update["clients"]
        if client["certificate_id"] == first_client["certificate_id"]
    )
    second_after_update = next(
        client for client in paired_after_update["clients"]
        if client["certificate_id"] == second_client["certificate_id"]
    )
    assert first_after_update.get("config_overrides", {}).get("fec_percentage") == 47, first_after_update
    assert first_after_update.get("config_overrides", {}).get("stream_audio") is True, first_after_update
    assert first_after_update.get("config_overrides", {}).get("audio_sink") == "client-endpoint", first_after_update
    assert first_after_update.get("config_overrides", {}).get("virtual_sink") == "client-virtual-endpoint", first_after_update
    assert first_after_update.get("config_overrides", {}).get("audio_sink_capture_only") is False, first_after_update
    assert first_after_update.get("config_overrides", {}).get("back_button_timeout") == -1, first_after_update
    assert {key: first_after_update["config_overrides"][key] for key in ("controller", "gamepad", "keyboard", "mouse")} == {"controller": False, "gamepad": "auto", "keyboard": False, "mouse": True}, first_after_update
    assert second_after_update.get("config_overrides", {}).get("fec_percentage") is None, second_after_update
    assert second_after_update.get("config_overrides", {}).get("audio_sink_capture_only") is None, second_after_update
    assert second_after_update.get("config_overrides", {}).get("virtual_sink") is None, second_after_update
    assert second_after_update.get("config_overrides", {}).get("back_button_timeout") is None, second_after_update
    assert all(key not in second_after_update.get("config_overrides", {}) for key in ("controller", "gamepad", "keyboard", "mouse")), second_after_update
    for invalid_input in ({"controller": "false"}, {"keyboard": "false"}, {"mouse": 1}, {"gamepad": "unknown-profile"}):
        status, rejected_input, _ = api.post(
            "/v1/pairing/update_client_config_overrides",
            {
                "certificate_id": first_client["certificate_id"],
                "expected_revision": paired_after_update["revision"],
                "config_overrides": invalid_input,
            },
            administrator,
        )
        assert status == 400 and not rejected_input["ok"], rejected_input
    for invalid_timeout in (2147483648, -2147483649, 0.5, "300"):
        status, rejected_timeout, _ = api.post(
            "/v1/pairing/update_client_config_overrides",
            {
                "certificate_id": first_client["certificate_id"],
                "expected_revision": paired_after_update["revision"],
                "config_overrides": {"back_button_timeout": invalid_timeout},
            },
            administrator,
        )
        assert status == 400 and not rejected_timeout["ok"], rejected_timeout
    for invalid_virtual_sink in ("line\nbreak", "x" * 4097, 1):
        status, rejected_sink, _ = api.post(
            "/v1/pairing/update_client_config_overrides",
            {
                "certificate_id": first_client["certificate_id"],
                "expected_revision": paired_after_update["revision"],
                "config_overrides": {"virtual_sink": invalid_virtual_sink},
            },
            administrator,
        )
        assert status == 400 and not rejected_sink["ok"], rejected_sink
    status, stale_fec_update, _ = api.post(
        "/v1/pairing/update_client_config_overrides",
        {
            "certificate_id": first_client["certificate_id"],
            "expected_revision": paired_clients["revision"],
            "config_overrides": {"fec_percentage": 61},
        },
        administrator,
    )
    assert status == 200 and not stale_fec_update["ok"], stale_fec_update
    assert stale_fec_update["code"] == "revision_conflict", stale_fec_update
    status, paired_after_conflict, _ = api.post("/v1/pairing/list_clients", token=administrator)
    assert status == 200 and paired_after_conflict["ok"], paired_after_conflict
    persisted_client = next(
        client for client in paired_after_conflict["clients"]
        if client["certificate_id"] == first_client["certificate_id"]
    )
    assert persisted_client.get("config_overrides", {}).get("fec_percentage") == 47, persisted_client
    assert persisted_client.get("config_overrides", {}).get("stream_audio") is True, persisted_client
    assert persisted_client.get("config_overrides", {}).get("audio_sink") == "client-endpoint", persisted_client
    assert persisted_client.get("config_overrides", {}).get("virtual_sink") == "client-virtual-endpoint", persisted_client
    assert persisted_client.get("config_overrides", {}).get("audio_sink_capture_only") is False, persisted_client
    assert persisted_client.get("config_overrides", {}).get("back_button_timeout") == -1, persisted_client
    assert {key: persisted_client["config_overrides"][key] for key in ("controller", "gamepad", "keyboard", "mouse")} == {"controller": False, "gamepad": "auto", "keyboard": False, "mouse": True}, persisted_client
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
    game_path = r"C:\Games\HTTPGame\game.exe" if os.name == "nt" else "/opt/games/http-game"
    steam_install_dir = r"C:\Games\HTTPGame" if os.name == "nt" else "/opt/games/http-game"
    prepare_path = r"C:\Tools\prepare.exe" if os.name == "nt" else "/opt/tools/prepare"
    entry = {
        "name": "HTTP Game",
        "elevated": True,
        "steam_id": "730",
        "steam_install_dir": steam_install_dir,
        "exclude_global_prep_cmd": True,
        "config_overrides": {"fec_percentage": 37, "stream_audio": False, "audio_sink": "", "virtual_sink": "", "audio_sink_capture_only": True, "back_button_timeout": 250, "controller": True, "gamepad": "auto", "keyboard": True, "mouse": False},
        "launch": {
            "kind": "executable",
            "program": game_path,
            "arguments": ["--fullscreen"],
            "auto_detach": False,
            "wait_all": True,
            "exit_timeout_seconds": 0,
            "detached": ["helper --ordered"],
            "preparation": [{"run": {"executable": prepare_path}}],
        },
    }
    status, created, _ = api.post("/v1/catalog/create", {"entry": entry, "expected_revision": catalog["revision"]}, administrator)
    assert status == 200 and created["ok"], created
    status, found, _ = api.post("/v1/catalog/get", {"id": created["id"]}, administrator)
    assert status == 200 and found["entry"]["config_overrides"] == entry["config_overrides"], found
    assert found["entry"]["elevated"] is True, found
    assert found["entry"]["steam_id"] == "730", found
    assert found["entry"]["steam_install_dir"] == steam_install_dir, found
    assert found["entry"]["exclude_global_prep_cmd"] is True, found
    assert found["entry"]["launch"]["auto_detach"] is False, found
    assert found["entry"]["launch"]["wait_all"] is True, found
    assert found["entry"]["launch"]["exit_timeout_seconds"] == 0, found
    assert found["entry"]["launch"]["detached"] == ["helper --ordered"], found
    assert found["entry"]["launch"]["preparation"][0]["run"]["executable"] == prepare_path, found
    for invalid_timeout in (2147483648, -2147483649, 0.5, "300"):
        status, rejected_timeout, _ = api.post(
            "/v1/catalog/create",
            {
                "entry": {**entry, "config_overrides": {"back_button_timeout": invalid_timeout}},
                "expected_revision": created["revision"],
            },
            administrator,
        )
        assert status == 400 and not rejected_timeout["ok"], rejected_timeout
    for invalid_input in ({"controller": "false"}, {"keyboard": "false"}, {"mouse": 1}, {"gamepad": "unknown-profile"}):
        status, rejected_input, _ = api.post(
            "/v1/catalog/create",
            {
                "entry": {**entry, "config_overrides": invalid_input},
                "expected_revision": created["revision"],
            },
            administrator,
        )
        assert status == 400 and not rejected_input["ok"], rejected_input
    for invalid_virtual_sink in ("line\nbreak", "x" * 4097, 1):
        status, rejected_sink, _ = api.post(
            "/v1/catalog/create",
            {
                "entry": {**entry, "config_overrides": {"virtual_sink": invalid_virtual_sink}},
                "expected_revision": created["revision"],
            },
            administrator,
        )
        assert status == 400 and not rejected_sink["ok"], rejected_sink
    for invalid_exclusion in (None, 1, "true"):
        status, rejected_exclusion, _ = api.post(
            "/v1/catalog/create",
            {
                "entry": {**entry, "exclude_global_prep_cmd": invalid_exclusion},
                "expected_revision": created["revision"],
            },
            administrator,
        )
        assert status == 400 and not rejected_exclusion["ok"], rejected_exclusion
    for invalid_metadata in ({"steam_id": "000730"}, {"steam_install_dir": "C:\\" if os.name == "nt" else "/"}):
        status, rejected, _ = api.post("/v1/catalog/create", {
            "entry": {**entry, **invalid_metadata}, "expected_revision": created["revision"],
        }, administrator)
        assert status == 400 and not rejected["ok"], rejected
    updated_entry = {
        **entry,
        "name": "Renamed",
        "config_overrides": {"fec_percentage": 43, "stream_audio": False, "audio_sink": "", "virtual_sink": "", "audio_sink_capture_only": True, "back_button_timeout": 250, "controller": True, "gamepad": "auto", "keyboard": True, "mouse": False},
        "launch": {**entry["launch"], "auto_detach": True, "wait_all": False},
    }
    status, renamed, _ = api.post("/v1/catalog/update", {"id": created["id"], "entry": updated_entry, "expected_revision": created["revision"]}, administrator)
    assert status == 200 and renamed["ok"], renamed
    status, conflict, _ = api.post(
        "/v1/catalog/update",
        {"id": created["id"], "entry": entry, "expected_revision": created["revision"]},
        administrator,
    )
    assert status == 200 and not conflict["ok"], conflict
    assert conflict["error"]["code"] == "revision_conflict", conflict
    status, saved_catalog_entry, _ = api.post("/v1/catalog/get", {"id": created["id"]}, administrator)
    assert status == 200 and saved_catalog_entry["entry"]["name"] == "Renamed", saved_catalog_entry
    assert saved_catalog_entry["entry"]["elevated"] is True, saved_catalog_entry
    assert saved_catalog_entry["entry"]["steam_id"] == "730", saved_catalog_entry
    assert saved_catalog_entry["entry"]["steam_install_dir"] == steam_install_dir, saved_catalog_entry
    assert saved_catalog_entry["entry"]["exclude_global_prep_cmd"] is True, saved_catalog_entry
    assert saved_catalog_entry["entry"]["config_overrides"] == updated_entry["config_overrides"], saved_catalog_entry
    assert saved_catalog_entry["entry"]["launch"]["auto_detach"] is True, saved_catalog_entry
    assert saved_catalog_entry["entry"]["launch"]["wait_all"] is False, saved_catalog_entry
    assert saved_catalog_entry["entry"]["launch"]["exit_timeout_seconds"] == 0, saved_catalog_entry
    assert saved_catalog_entry["entry"]["launch"]["detached"] == ["helper --ordered"], saved_catalog_entry
    catalog_id = created["id"]
    catalog_revision = renamed["revision"]
    catalog_entry = saved_catalog_entry["entry"]
    fullscreen_entry = {
        "name": "Manual Playnite Fullscreen",
        "launch": {"kind": "playnite_fullscreen", "exit_timeout_seconds": 0},
    }
    status, fullscreen_created, _ = api.post(
        "/v1/catalog/create",
        {"entry": fullscreen_entry, "expected_revision": catalog_revision},
        administrator,
    )
    assert status == 200 and fullscreen_created["ok"], fullscreen_created
    status, fullscreen_saved, _ = api.post(
        "/v1/catalog/get", {"id": fullscreen_created["id"]}, administrator
    )
    assert status == 200 and fullscreen_saved["entry"]["launch"] == fullscreen_entry["launch"], fullscreen_saved
    fullscreen_updated_entry = {
        **fullscreen_entry,
        "launch": {"kind": "playnite_fullscreen", "exit_timeout_seconds": 300},
    }
    status, fullscreen_updated, _ = api.post(
        "/v1/catalog/update",
        {
            "id": fullscreen_created["id"],
            "entry": fullscreen_updated_entry,
            "expected_revision": fullscreen_created["revision"],
        },
        administrator,
    )
    assert status == 200 and fullscreen_updated["ok"], fullscreen_updated
    status, fullscreen_saved, _ = api.post(
        "/v1/catalog/get", {"id": fullscreen_created["id"]}, administrator
    )
    assert status == 200 and fullscreen_saved["entry"]["launch"] == fullscreen_updated_entry["launch"], fullscreen_saved
    catalog_revision = fullscreen_updated["revision"]
    for invalid_launch in (
        {"kind": "playnite_fullscreen", "exit_timeout_seconds": 301},
        {"kind": "playnite_fullscreen", "exit_timeout_seconds": 10, "path": game_path},
        {"kind": "playnite_fullscreen"},
    ):
        status, rejected, _ = api.post(
            "/v1/catalog/create",
            {"entry": {"name": "Invalid Fullscreen", "launch": invalid_launch}, "expected_revision": catalog_revision},
            administrator,
        )
        assert status == 400 and not rejected["ok"], rejected
    for invalid_elevation in (
        {**entry, "elevated": "true"},
        {**fullscreen_entry, "elevated": True},
    ):
        status, rejected, _ = api.post(
            "/v1/catalog/create",
            {"entry": invalid_elevation, "expected_revision": catalog_revision},
            administrator,
        )
        assert status == 400 and not rejected["ok"], rejected
    playnite_override_id, playnite_override_value, catalog_revision = test_provider_override_route(
        api, administrator, catalog_id
    )
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
    assert host_settings["address_family"] == "ipv4", host_settings
    assert host_settings["bind_address"] == "127.0.0.1", host_settings
    assert host_settings["configured_address_family"] is None, host_settings
    assert host_settings["configured_bind_address"] is None, host_settings
    assert host_settings["configured_output_name"] is None, host_settings
    assert host_settings["configured_capture"] == "auto", host_settings
    assert host_settings["configured_video_max_batch_size_kb"] is None, host_settings
    assert host_settings["effective_video_max_batch_size_kb"] == 64, host_settings
    assert host_settings["video_batch_size_fallback"] is False, host_settings
    assert host_settings["configured_max_bitrate"] is None, host_settings
    assert host_settings["effective_max_bitrate_kbps"] is None, host_settings
    assert host_settings["configured_minimum_fps_target"] is None, host_settings
    assert host_settings["effective_minimum_fps_target"] == 20, host_settings
    assert host_settings["configured_audio_sink"] is None, host_settings
    assert host_settings["configured_virtual_sink"] is None, host_settings
    assert host_settings["configured_global_preparation"] == [], host_settings
    assert host_settings["configured_external_ip"] is None, host_settings
    assert isinstance(host_settings["effective_audio_sink"], str), host_settings
    assert host_settings["configured_wgc_pacing_smoothing"] is None, host_settings
    assert host_settings["effective_wgc_pacing_smoothing"] is True, host_settings
    assert host_settings["configured_ping_timeout_ms"] is None, host_settings
    assert host_settings["effective_ping_timeout_ms"] == 10_000, host_settings
    assert host_settings["stream_audio"] is True, host_settings
    assert host_settings["audio_sink_capture_only"] is False, host_settings
    assert host_settings["http_addresses"] == [f"127.0.0.1:{host_settings['port']}"], host_settings
    assert host_settings["https_addresses"] == [f"127.0.0.1:{host_settings['https_port']}"], host_settings
    assert host_settings["rtsp_addresses"] == [f"127.0.0.1:{host_settings['port'] + 21}"], host_settings
    status, default_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "", "port": host_settings["port"],
        "address_family": None, "bind_address": None,
        "expected_revision": host_settings["revision"],
    }, administrator)
    assert status == 200 and default_host["configured_hostname"] == "" and default_host["hostname"], default_host
    assert default_host["configured_address_family"] is None, default_host
    assert default_host["configured_bind_address"] is None, default_host
    connection = http.client.HTTPConnection(host, int(port), timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        default_info = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert default_info.findtext("hostname") == default_host["hostname"], ET.tostring(default_info)
    status, explicit_blank_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "", "port": default_host["port"],
        "address_family": "ipv4", "bind_address": "",
        "expected_revision": default_host["revision"],
    }, administrator)
    assert status == 200 and explicit_blank_host["ok"], explicit_blank_host
    assert explicit_blank_host["configured_address_family"] == "ipv4", explicit_blank_host
    assert explicit_blank_host["configured_bind_address"] == "", explicit_blank_host
    assert explicit_blank_host["bind_address"] == "127.0.0.1", explicit_blank_host
    global_preparation = [{"run": {"executable": prepare_path}}]
    status, renamed_host, _ = api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games",
        "port": explicit_blank_host["port"],
        "output_name": "test-output",
        "capture": "wgcc",
        "video_max_batch_size_kb": 20,
        "max_bitrate": 50_000,
        "minimum_fps_target": 37.5,
        "audio_sink": "test-audio-endpoint",
        "virtual_sink": "test-virtual-endpoint",
        "global_preparation": global_preparation,
        "audio_sink_capture_only": True,
        "external_ip": "203.0.113.42",
        "wgc_pacing_smoothing": False,
        "ping_timeout_ms": 2300,
        "stream_audio": False,
        "expected_revision": explicit_blank_host["revision"],
    }, administrator)
    assert status == 200 and renamed_host["hostname"] == "Living Room & Games", renamed_host
    assert renamed_host["configured_output_name"] == "test-output", renamed_host
    assert renamed_host["effective_output_name"] == "test-output", renamed_host
    assert renamed_host["configured_capture"] == "wgcc", renamed_host
    assert renamed_host["configured_video_max_batch_size_kb"] == 20, renamed_host
    assert renamed_host["effective_video_max_batch_size_kb"] == 64, renamed_host
    assert renamed_host["video_batch_size_fallback"] is True, renamed_host
    assert renamed_host["configured_max_bitrate"] == 50_000, renamed_host
    assert renamed_host["effective_max_bitrate_kbps"] == 50_000, renamed_host
    assert renamed_host["configured_minimum_fps_target"] == 37.5, renamed_host
    assert renamed_host["effective_minimum_fps_target"] == 37.5, renamed_host
    assert renamed_host["configured_audio_sink"] == "test-audio-endpoint", renamed_host
    assert renamed_host["configured_virtual_sink"] == "test-virtual-endpoint", renamed_host
    assert len(renamed_host["configured_global_preparation"]) == 1, renamed_host
    assert renamed_host["configured_global_preparation"][0]["run"]["executable"] == prepare_path, renamed_host
    assert renamed_host["audio_sink_capture_only"] is True, renamed_host
    assert renamed_host["configured_external_ip"] == "203.0.113.42", renamed_host
    assert renamed_host["effective_audio_sink"] == "test-audio-endpoint", renamed_host
    assert renamed_host["configured_wgc_pacing_smoothing"] is False, renamed_host
    assert renamed_host["effective_wgc_pacing_smoothing"] is False, renamed_host
    assert renamed_host["configured_ping_timeout_ms"] == 2300, renamed_host
    assert renamed_host["effective_ping_timeout_ms"] == 2300, renamed_host
    assert renamed_host["stream_audio"] is False, renamed_host
    assert renamed_host["configured_address_family"] == "ipv4", renamed_host
    assert renamed_host["configured_bind_address"] == "", renamed_host
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
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "output_name": " ",
    }, administrator)[0] == 400
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "capture": "dxgi",
    }, administrator)[0] == 400
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "video_max_batch_size_kb": 65,
    }, administrator)[0] == 400
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "max_bitrate": 2_147_483_648,
    }, administrator)[0] == 400
    for invalid_target in (-0.1, 1000.1, "20"):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "minimum_fps_target": invalid_target,
        }, administrator)[0] == 400
    for invalid_sink in ("bad\nendpoint", "x" * 4097, 42):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "audio_sink": invalid_sink,
        }, administrator)[0] == 400
    for invalid_sink in ("bad\nvirtual endpoint", "x" * 4097, 42):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "virtual_sink": invalid_sink,
        }, administrator)[0] == 400
    for invalid_preparation in (False, {}, [{"run": {"executable": "relative-tool"}}]):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "global_preparation": invalid_preparation,
        }, administrator)[0] == 400
    for invalid_capture_only in (None, 1, "true"):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "audio_sink_capture_only": invalid_capture_only,
        }, administrator)[0] == 400
    for invalid_external_ip in ("bad\naddress", "x" * 4097, 42):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "external_ip": invalid_external_ip,
        }, administrator)[0] == 400
    assert api.post("/v1/nvhttp/settings_update", {
        "hostname": "Living Room & Games", "port": renamed_host["port"],
        "expected_revision": renamed_host["revision"],
        "wgc_pacing_smoothing": "false",
    }, administrator)[0] == 400
    for invalid_timeout in (-2, 2_147_483_648, "2300"):
        assert api.post("/v1/nvhttp/settings_update", {
            "hostname": "Living Room & Games", "port": renamed_host["port"],
            "expected_revision": renamed_host["revision"],
            "ping_timeout_ms": invalid_timeout,
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
    assert moved_host["configured_output_name"] == "test-output", moved_host
    assert moved_host["configured_capture"] == "wgcc", moved_host
    assert moved_host["configured_video_max_batch_size_kb"] == 20, moved_host
    assert moved_host["configured_max_bitrate"] == 50_000, moved_host
    assert moved_host["effective_max_bitrate_kbps"] == 50_000, moved_host
    assert moved_host["configured_minimum_fps_target"] == 37.5, moved_host
    assert moved_host["effective_minimum_fps_target"] == 37.5, moved_host
    assert moved_host["configured_audio_sink"] == "test-audio-endpoint", moved_host
    assert moved_host["configured_global_preparation"] == renamed_host["configured_global_preparation"], moved_host
    assert moved_host["audio_sink_capture_only"] is True, moved_host
    assert moved_host["configured_external_ip"] == "203.0.113.42", moved_host
    assert moved_host["effective_audio_sink"] == "test-audio-endpoint", moved_host
    assert moved_host["configured_wgc_pacing_smoothing"] is False, moved_host
    assert moved_host["effective_wgc_pacing_smoothing"] is False, moved_host
    assert moved_host["configured_ping_timeout_ms"] == 2300, moved_host
    assert moved_host["effective_ping_timeout_ms"] == 2300, moved_host
    assert moved_host["stream_audio"] is False, moved_host
    assert moved_host["active_http_port"] == moved_port, moved_host
    assert moved_host["active_https_port"] == moved_port - 5, moved_host
    connection = http.client.HTTPConnection(host, moved_port, timeout=5)
    try:
        connection.request("GET", "/serverinfo")
        moved_info = ET.fromstring(connection.getresponse().read())
    finally:
        connection.close()
    assert moved_info.findtext("hostname") == "Living Room & Games", ET.tostring(moved_info)
    status, input_defaults, _ = api.post("/v1/input/settings_get", token=administrator)
    assert status == 200 and all(
        input_defaults["values"].get(key) is True
        for key in (
            "keyboard",
            "mouse",
            "high_resolution_scrolling",
            "always_send_scancodes",
        )
    ), input_defaults
    assert input_defaults["values"]["key_rightalt_to_key_win"] is False, input_defaults
    assert input_defaults["values"]["key_repeat_delay"] == 500, input_defaults
    assert input_defaults["values"]["key_repeat_frequency"] == 24.9, input_defaults
    assert input_defaults["values"]["back_button_timeout"] == -1, input_defaults
    assert input_defaults["values"]["gamepad"] == "auto", input_defaults
    assert input_defaults["values"]["controller"] is True, input_defaults
    assert input_defaults["revision"] == 0 and input_defaults["persisted"] and input_defaults["applied"], input_defaults
    selected_gamepad = "x360" if sys.platform == "win32" else "xone" if sys.platform == "linux" else "auto"
    assert api.post("/v1/input/status")[0] == 401
    status, input_status, _ = api.post("/v1/input/status", token=administrator)
    assert status == 200 and input_status["gamepad"] == "auto", input_status
    assert input_status["controller_enabled"] is True, input_status
    assert "auto" in input_status["supported_profiles"], input_status
    assert selected_gamepad in input_status["supported_profiles"], input_status
    input_policy_update = {
        "keyboard": False,
        "mouse": True,
        "high_resolution_scrolling": False,
        "always_send_scancodes": False,
        "key_rightalt_to_key_win": True,
        "key_repeat_delay": 275,
        "key_repeat_frequency": 31.25,
        "back_button_timeout": 300,
        "gamepad": selected_gamepad,
        "controller": False,
    }
    input_values = {**input_defaults["values"], **input_policy_update}
    status, input_updated, _ = api.post("/v1/input/settings_update", {
        **input_policy_update, "expected_revision": input_defaults["revision"],
    }, administrator)
    assert status == 200 and input_updated["ok"] and input_updated["persisted"] and input_updated["applied"], input_updated
    assert input_updated["values"] == input_values and input_updated["revision"] == 1, input_updated
    status, input_status, _ = api.post("/v1/input/status", token=administrator)
    assert status == 200 and input_status["gamepad"] == selected_gamepad, input_status
    assert input_status["controller_enabled"] is False, input_status
    status, input_conflict, _ = api.post("/v1/input/settings_update", {
        "keyboard": True,
        "mouse": True,
        "high_resolution_scrolling": True,
        "always_send_scancodes": True,
        "key_rightalt_to_key_win": False,
        "expected_revision": input_defaults["revision"],
    }, administrator)
    assert status == 200 and input_conflict.get("error", {}).get("code") == "revision_conflict", input_conflict
    assert api.post("/v1/input/settings_update", {
        "keyboard": True, "mouse": True, "expected_revision": 1, "owner": "amaterasu.other",
    }, administrator)[0] == 400
    for invalid_repeat in [
        {"key_repeat_delay": -1},
        {"key_repeat_delay": 1.5},
        {"key_repeat_delay": 2147483648},
        {"key_repeat_frequency": -0.1},
        {"back_button_timeout": 2147483648},
        {"back_button_timeout": -2147483649},
        {"back_button_timeout": 0.5},
        {"back_button_timeout": None},
        {"gamepad": "not_a_gamepad"},
        {"gamepad": 1},
        {"controller": None},
        {"controller": "false"},
    ]:
        invalid_status, _, _ = api.post("/v1/input/settings_update", {
            "keyboard": input_values["keyboard"],
            "mouse": input_values["mouse"],
            **invalid_repeat,
            "expected_revision": input_updated["revision"],
        }, administrator)
        assert invalid_status == 400, invalid_repeat
    zero_repeat = {"key_repeat_delay": 0, "key_repeat_frequency": 0}
    status, input_zero_repeat, _ = api.post("/v1/input/settings_update", {
        "keyboard": input_values["keyboard"],
        "mouse": input_values["mouse"],
        **zero_repeat,
        "expected_revision": input_updated["revision"],
    }, administrator)
    input_values = {**input_values, **zero_repeat}
    assert status == 200 and input_zero_repeat["ok"] and input_zero_repeat["applied"], input_zero_repeat
    assert input_zero_repeat["values"] == input_values and input_zero_repeat["revision"] == 2, input_zero_repeat
    status, input_saved, _ = api.post("/v1/input/settings_get", token=administrator)
    assert status == 200 and input_saved["values"] == input_values and input_saved["revision"] == 2, input_saved
    # A v3 client omits Right Alt, repeat, controller, Back timeout, and gamepad fields; preserve them while it
    # updates the older keyboard, mouse, scrolling and scancode settings.
    status, input_legacy_updated, _ = api.post("/v1/input/settings_update", {
        "keyboard": True,
        "mouse": True,
        "high_resolution_scrolling": input_saved["values"]["high_resolution_scrolling"],
        "always_send_scancodes": input_saved["values"]["always_send_scancodes"],
        "expected_revision": input_saved["revision"],
    }, administrator)
    input_values = {**input_values, "keyboard": True, "mouse": True}
    assert status == 200 and input_legacy_updated["ok"] and input_legacy_updated["applied"], input_legacy_updated
    assert input_legacy_updated["values"] == input_values and input_legacy_updated["revision"] == 3, input_legacy_updated
    input_revision = input_legacy_updated["revision"]
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
        "autosync_remove_uninstalled": True,
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
        "sync_all_installed": True,
        "recent_games": 25,
        "recent_max_age_days": 45,
        "autosync_remove_uninstalled": False,
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
    artwork_revision = games["games"][0].get("artwork_revision")
    assert isinstance(artwork_revision, str) and artwork_revision.isdecimal(), games
    status, artwork, _ = api.post("/v1/steam/artwork_read", {
        "app_id": 730, "revision": artwork_revision, "offset": 0,
    }, administrator)
    assert status == 200 and artwork["ok"], artwork
    assert artwork["next_offset"] is None and artwork["total_bytes"] == len(STEAM_ARTWORK), artwork
    assert artwork["mime_type"] == "image/png", artwork
    assert base64.b64decode(artwork["data"]) == STEAM_ARTWORK, artwork
    assert api.post("/v1/steam/artwork_read", {
        "app_id": 730, "revision": artwork_revision, "offset": 1,
    }, administrator)[0] == 400
    deadline = time.monotonic() + 5
    while True:
        status, lutris_status, _ = api.post("/v1/lutris/status", token=administrator)
        assert status == 200 and lutris_status["ok"], lutris_status
        if lutris_status["settings_ready"]:
            break
        assert time.monotonic() < deadline, lutris_status
        time.sleep(0.02)
    status, lutris_defaults, _ = api.post("/v1/lutris/settings", token=administrator)
    assert status == 200 and lutris_defaults["ok"], lutris_defaults
    linux_runtime = sys.platform.startswith("linux")
    assert lutris_status["platform_supported"] == linux_runtime, lutris_status
    assert lutris_defaults["values"]["enabled"] == linux_runtime, lutris_defaults
    lutris_values = {
        "enabled": lutris_defaults["values"]["enabled"],
        "auto_sync": False,
        "autosync_remove_uninstalled": False,
        "include_steam": False,
        "exclude_games": [
            {"id": "730", "name": "Old game name"},
            {"name": "Excluded by name"},
        ],
    }
    status, lutris_saved, _ = api.post(
        "/v1/lutris/settings_update",
        {**lutris_values, "expected_revision": lutris_defaults["revision"]},
        administrator,
    )
    assert status == 200 and lutris_saved["ok"], lutris_saved
    assert lutris_saved["values"] == lutris_values, lutris_saved
    status, lutris_conflict, _ = api.post(
        "/v1/lutris/settings_update",
        {
            **lutris_values,
            "auto_sync": True,
            "expected_revision": lutris_defaults["revision"],
        },
        administrator,
    )
    assert status == 200 and not lutris_conflict["ok"], lutris_conflict
    assert lutris_conflict.get("error", {}).get("code") == "revision_conflict", lutris_conflict
    status, lutris_readback, _ = api.post("/v1/lutris/settings", token=administrator)
    assert status == 200 and lutris_readback["values"] == lutris_values, lutris_readback
    lutris_revision = lutris_readback["revision"]
    if not linux_runtime:
        status, rejected_lutris_enable, _ = api.post(
            "/v1/lutris/settings_update",
            {**lutris_values, "enabled": True, "expected_revision": lutris_revision},
            administrator,
        )
        assert status == 200 and not rejected_lutris_enable["ok"], rejected_lutris_enable
        assert rejected_lutris_enable.get("error", {}).get("code") == "invalid_request", rejected_lutris_enable
        status, lutris_readback, _ = api.post("/v1/lutris/settings", token=administrator)
        assert status == 200 and lutris_readback["values"] == lutris_values, lutris_readback
        status, rejected_refresh, _ = api.post("/v1/lutris/refresh", token=administrator)
        assert status == 200 and not rejected_refresh["ok"], rejected_refresh
        assert rejected_refresh["code"] == "lutris_disabled", rejected_refresh
    lutris_revision = lutris_readback["revision"]
    deadline = time.monotonic() + 5
    while True:
        status, playnite_status, _ = api.post("/v1/playnite/status", token=administrator)
        assert status == 200 and playnite_status["ok"], playnite_status
        if playnite_status["settings_ready"]:
            break
        assert time.monotonic() < deadline, playnite_status
        time.sleep(0.02)
    status, playnite_defaults, _ = api.post("/v1/playnite/settings", token=administrator)
    assert status == 200 and playnite_defaults["ok"], playnite_defaults
    assert playnite_defaults["values"] == {
        "enabled": sys.platform == "win32",
        "auto_sync": True,
        "sync_all_installed": False,
        "autosync_remove_uninstalled": True,
        "autosync_delete_after_days": 14,
        "autosync_require_replacement": True,
        "focus_attempts": 3,
        "focus_timeout_secs": 15,
        "focus_exit_on_first": False,
        "fullscreen_entry_enabled": False,
        "recent_games": 10,
        "recent_max_age_days": 30,
        "sync_categories": [],
        "exclude_categories": [],
        "sync_plugins": [],
        "exclude_plugins": [],
        "exclude_games": [],
    }, playnite_defaults
    playnite_values = {
        "enabled": False,
        "auto_sync": False,
        "sync_all_installed": True,
        "autosync_remove_uninstalled": False,
        "autosync_delete_after_days": 0,
        "autosync_require_replacement": False,
        "focus_attempts": 5,
        "focus_timeout_secs": 20,
        "focus_exit_on_first": True,
        "fullscreen_entry_enabled": False,
        "recent_games": 25,
        "recent_max_age_days": 45,
        "sync_categories": ["Action", "RPG"],
        "exclude_categories": ["Excluded category"],
        "sync_plugins": ["plugin.include"],
        "exclude_plugins": ["plugin.exclude"],
        "exclude_games": ["game-excluded"],
    }
    status, playnite_saved, _ = api.post(
        "/v1/playnite/settings_update",
        {**playnite_values, "expected_revision": playnite_defaults["revision"]},
        administrator,
    )
    assert status == 200 and playnite_saved["ok"], playnite_saved
    assert playnite_saved["values"] == playnite_values, playnite_saved
    playnite_revision = playnite_saved["revision"]
    status, playnite_conflict, _ = api.post(
        "/v1/playnite/settings_update",
        {"enabled": True, "expected_revision": playnite_defaults["revision"]},
        administrator,
    )
    assert status == 200 and playnite_conflict.get("error", {}).get("code") == "revision_conflict", playnite_conflict
    status, playnite_readback, _ = api.post("/v1/playnite/settings", token=administrator)
    assert status == 200 and playnite_readback["values"] == playnite_values, playnite_readback
    assert playnite_readback["revision"] == playnite_revision, playnite_readback
    status, blocked_playnite_refresh, _ = api.post("/v1/playnite/refresh", token=administrator)
    assert status == 200 and not blocked_playnite_refresh["ok"], blocked_playnite_refresh
    assert blocked_playnite_refresh["code"] == "playnite_disabled", blocked_playnite_refresh
    if sys.platform == "win32":
        status, fullscreen_settings, _ = api.post(
            "/v1/playnite/settings_update",
            {
                **playnite_values,
                "enabled": True,
                "fullscreen_entry_enabled": True,
                "expected_revision": playnite_revision,
            },
            administrator,
        )
        assert status == 200 and fullscreen_settings["ok"], fullscreen_settings
        synthetic = None
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            status, fullscreen_catalog, _ = api.post("/v1/catalog/list", token=administrator)
            assert status == 200 and fullscreen_catalog["ok"], fullscreen_catalog
            synthetic = next(
                (
                    row for row in fullscreen_catalog["entries"]
                    if row["entry"]["name"] == "Playnite (Fullscreen)"
                    and row["entry"]["launch"]["kind"] == "playnite_fullscreen"
                ),
                None,
            )
            if synthetic is not None:
                break
            time.sleep(0.05)
        assert synthetic is not None, "Playnite Fullscreen entry was not created"
        status, managed, _ = api.post(
            "/v1/catalog/delete",
            {"id": synthetic["id"], "expected_revision": fullscreen_catalog["revision"]},
            administrator,
        )
        assert status == 200 and managed.get("error", {}).get("code") == "provider_entry_managed", managed
        status, restored_settings, _ = api.post(
            "/v1/playnite/settings_update",
            {**playnite_values, "expected_revision": fullscreen_settings["revision"]},
            administrator,
        )
        assert status == 200 and restored_settings["ok"], restored_settings
        playnite_revision = restored_settings["revision"]
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            status, fullscreen_catalog, _ = api.post("/v1/catalog/list", token=administrator)
            assert status == 200 and fullscreen_catalog["ok"], fullscreen_catalog
            if all(row["id"] != synthetic["id"] for row in fullscreen_catalog["entries"]):
                break
            time.sleep(0.05)
        assert all(row["id"] != synthetic["id"] for row in fullscreen_catalog["entries"]), fullscreen_catalog
        catalog_revision = fullscreen_catalog["revision"]
    token = console.request("amaterasu.authentication/1", {"action": "issue_token", "scopes": ["catalog.read"], "ttl_seconds": 3600})
    assert token["ok"], token
    status, result, _ = api.post("/v1/catalog/list", token=token["bearer"])
    assert status == 200 and result["ok"], result
    assert api.post("/v1/lutris/settings", token=token["bearer"])[0] == 403
    assert api.post("/v1/lutris/status", token=token["bearer"])[0] == 403
    assert api.post("/v1/playnite/settings", token=token["bearer"])[0] == 403
    assert api.post("/v1/playnite/status", token=token["bearer"])[0] == 403
    assert api.post("/v1/playnite/refresh", token=token["bearer"])[0] == 403
    status, result, _ = api.post(
        "/v1/lutris/settings_update",
        {**lutris_values, "expected_revision": lutris_revision},
        token["bearer"],
    )
    assert status == 403 and not result["ok"], result
    assert api.post("/v1/lutris/refresh", token=token["bearer"])[0] == 403
    assert api.post("/v1/pairing/list_clients", token=token["bearer"])[0] == 403
    status, result, _ = api.post(
        "/v1/pairing/update_client_config_overrides",
        {
            "certificate_id": first_client["certificate_id"],
            "expected_revision": paired_after_conflict["revision"],
            "config_overrides": {"fec_percentage": 53},
        },
        token["bearer"],
    )
    assert status == 403 and not result["ok"], result
    status, result, _ = api.post("/v1/catalog/create", {"entry": entry, "expected_revision": catalog_revision}, token["bearer"])
    assert status == 403 and not result["ok"], result
    status, result, _ = api.post(
        "/v1/catalog/update_provider_overrides",
        {
            "id": catalog_id,
            "expected_revision": catalog_revision,
            "config_overrides": {"fec_percentage": 53},
        },
        token["bearer"],
    )
    assert status == 403 and not result["ok"], result
    assert api.post("/v1/nvhttp/settings", token=token["bearer"])[0] == 403
    assert api.post("/v1/steam/settings", token=token["bearer"])[0] == 403
    assert api.post("/v1/input/settings_get", token=token["bearer"])[0] == 403
    status, result, _ = api.post(
        "/v1/playnite/settings_update",
        {**playnite_values, "expected_revision": playnite_revision},
        token["bearer"],
    )
    assert status == 403 and not result["ok"], result
    assert token["bearer"] not in json.dumps(result)
    # Route actions cannot be swapped through payloads or arbitrary command dispatch.
    assert api.post("/v1/catalog/list", {"action": "create"}, token["bearer"])[0] == 400
    assert api.post("/v1/runtime/request", {"service": "amaterasu.state/1"}, administrator)[0] == 404
    return (
        token,
        administrator,
        moved_port,
        steam_values,
        auth_values,
        saved_auth_settings["revision"],
        input_values,
        input_revision,
        first_client["certificate_id"],
        lutris_values,
        lutris_revision,
        playnite_values,
        playnite_revision,
        catalog_id,
        catalog_revision,
        catalog_entry,
        playnite_override_id,
        playnite_override_value,
    )


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


def test_history_settings(api, administrator, reader):
    assert api.post("/v1/history/settings_get")[0] == 401
    assert api.post("/v1/history/settings_get", token=reader)[0] == 403
    status, initial, _ = api.post("/v1/history/settings_get", token=administrator)
    assert status == 200 and initial["ok"], initial
    assert initial["settings"] == {
        "enabled": True, "ttl_days": 0, "db_size_limit_mb": 0,
    }, initial
    status, updated, _ = api.post("/v1/history/settings_update", {
        "settings": {"enabled": True, "ttl_days": 7, "db_size_limit_mb": 16},
        "expected_revision": initial["revision"],
    }, administrator)
    assert status == 200 and updated["ok"], updated
    assert updated["revision"] == initial["revision"] + 1, updated
    assert updated["settings"]["ttl_days"] == 7, updated
    assert updated["settings"]["db_size_limit_mb"] == 16, updated
    assert api.post("/v1/history/settings_update", {
        "settings": {"enabled": True, "ttl_days": 8, "db_size_limit_mb": 16},
        "expected_revision": initial["revision"],
    }, administrator)[1]["code"] == "revision_conflict"
    assert api.post("/v1/history/settings_update", {
        "settings": {"enabled": True, "ttl_days": -1, "db_size_limit_mb": 0},
        "expected_revision": updated["revision"],
    }, administrator)[0] == 400
    status, listed, _ = api.post("/v1/history/list_sessions", {"limit": 25}, administrator)
    assert status == 200 and listed == {
        "ok": True, "sessions": [], "next_offset": None,
    }, listed
    status, history_status, _ = api.post("/v1/history/history_status", token=administrator)
    assert status == 200 and history_status["status"]["available"], history_status
    return updated["settings"], updated["revision"]


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-management-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure_steam_fixture(harness)
        artwork_path = harness.directory / "steam-library" / "appcache" / "librarycache" / "730_library_600x900_2x.png"
        artwork_path.parent.mkdir(parents=True)
        artwork_path.write_bytes(STEAM_ARTWORK)
        initial_port = available_moonlight_port_pair()
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["global"] = {
            "min_log_level": "warning",
            "log_path": "parity.log",
        }
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
            deadline = time.monotonic() + 20
            while True:
                logging_status = console.request("amaterasu.diagnostics/1", {
                    "action": "status",
                })
                assert logging_status["ok"], logging_status
                if (logging_status["ready"]
                        and logging_status["logging_configuration_revision"] is not None):
                    break
                assert time.monotonic() < deadline, logging_status
                time.sleep(0.05)
            assert console.request("amaterasu.diagnostics/1", {
                "action": "record", "level": "info", "target": "integration",
                "message": "filtered parity log record",
            })["ok"]
            assert console.request("amaterasu.diagnostics/1", {
                "action": "record", "level": "warn", "target": "integration",
                "message": "written parity log record",
            })["ok"]
            token, session, moved_port, steam_values, auth_values, auth_revision, input_values, input_revision, fec_client_id, lutris_values, lutris_revision, playnite_values, playnite_revision, catalog_id, catalog_revision, catalog_entry, playnite_override_id, playnite_override_value = test_auth_and_crud(console, api)
            history_values, history_revision = test_history_settings(api, session, token["bearer"])
            test_http_framing(api)
            console.eof()
        finally:
            console.close()
        log_files = list((harness.root / "data" / "state" / "logs").glob("parity*.log"))
        assert log_files, "configured diagnostics log path did not create a session file"
        log_text = "\n".join(path.read_text() for path in log_files)
        assert "written parity log record" in log_text, log_text
        assert "filtered parity log record" not in log_text, log_text
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
            status, input_settings, _ = api.post("/v1/input/settings_get", token=relogged["bearer"])
            assert status == 200 and input_settings["values"] == input_values, input_settings
            assert input_settings["revision"] == input_revision and input_settings["applied"], input_settings
            status, history_settings, _ = api.post("/v1/history/settings_get", token=relogged["bearer"])
            assert status == 200 and history_settings["settings"] == history_values, history_settings
            assert history_settings["revision"] == history_revision, history_settings
            status, host_settings, _ = api.post("/v1/nvhttp/settings", token=relogged["bearer"])
            assert status == 200 and host_settings["hostname"] == "Living Room & Games", host_settings
            assert host_settings["port"] == moved_port and host_settings["active_http_port"] == moved_port, host_settings
            assert host_settings["audio_sink_capture_only"] is True, host_settings
            assert host_settings["configured_virtual_sink"] == "test-virtual-endpoint", host_settings
            assert len(host_settings["configured_global_preparation"]) == 1, host_settings
            assert host_settings["configured_global_preparation"][0]["run"]["executable"] == (r"C:\Tools\prepare.exe" if os.name == "nt" else "/opt/tools/prepare"), host_settings
            assert host_settings["address_family"] == "ipv4", host_settings
            assert host_settings["configured_address_family"] == "ipv4", host_settings
            assert host_settings["configured_bind_address"] == "", host_settings
            assert host_settings["configured_output_name"] == "test-output", host_settings
            assert host_settings["configured_capture"] == "wgcc", host_settings
            assert host_settings["configured_video_max_batch_size_kb"] == 20, host_settings
            assert host_settings["effective_video_max_batch_size_kb"] == 64, host_settings
            assert host_settings["video_batch_size_fallback"] is True, host_settings
            assert host_settings["configured_max_bitrate"] == 50_000, host_settings
            assert host_settings["effective_max_bitrate_kbps"] == 50_000, host_settings
            assert host_settings["configured_minimum_fps_target"] == 37.5, host_settings
            assert host_settings["effective_minimum_fps_target"] == 37.5, host_settings
            assert host_settings["configured_audio_sink"] == "test-audio-endpoint", host_settings
            assert host_settings["configured_external_ip"] == "203.0.113.42", host_settings
            assert host_settings["effective_audio_sink"] == "test-audio-endpoint", host_settings
            assert host_settings["configured_wgc_pacing_smoothing"] is False, host_settings
            assert host_settings["effective_wgc_pacing_smoothing"] is False, host_settings
            assert host_settings["configured_ping_timeout_ms"] == 2300, host_settings
            assert host_settings["effective_ping_timeout_ms"] == 2300, host_settings
            assert host_settings["stream_audio"] is False, host_settings
            reset_revision = host_settings["revision"]
            status, steam_settings, _ = api.post("/v1/steam/settings", token=relogged["bearer"])
            assert status == 200 and steam_settings["values"] == steam_values, steam_settings
            status, paired_clients, _ = api.post("/v1/pairing/list_clients", token=relogged["bearer"])
            assert status == 200 and paired_clients["ok"], paired_clients
            persisted_client = next(
                client for client in paired_clients["clients"]
                if client["certificate_id"] == fec_client_id
            )
            assert persisted_client.get("config_overrides", {}).get("fec_percentage") == 47, persisted_client
            assert persisted_client.get("config_overrides", {}).get("stream_audio") is True, persisted_client
            assert persisted_client.get("config_overrides", {}).get("audio_sink") == "client-endpoint", persisted_client
            assert persisted_client.get("config_overrides", {}).get("virtual_sink") == "client-virtual-endpoint", persisted_client
            assert persisted_client.get("config_overrides", {}).get("audio_sink_capture_only") is False, persisted_client
            assert persisted_client.get("config_overrides", {}).get("back_button_timeout") == -1, persisted_client
            assert {key: persisted_client["config_overrides"][key] for key in ("controller", "gamepad", "keyboard", "mouse")} == {"controller": False, "gamepad": "auto", "keyboard": False, "mouse": True}, persisted_client
            status, lutris_settings, _ = api.post("/v1/lutris/settings", token=relogged["bearer"])
            assert status == 200 and lutris_settings["values"] == lutris_values, lutris_settings
            assert lutris_settings["revision"] == lutris_revision, lutris_settings
            status, lutris_status, _ = api.post("/v1/lutris/status", token=relogged["bearer"])
            assert status == 200 and lutris_status["platform_supported"] == sys.platform.startswith("linux"), lutris_status
            status, playnite_settings, _ = api.post("/v1/playnite/settings", token=relogged["bearer"])
            assert status == 200 and playnite_settings["values"] == playnite_values, playnite_settings
            assert playnite_settings["revision"] == playnite_revision, playnite_settings
            status, playnite_status, _ = api.post("/v1/playnite/status", token=relogged["bearer"])
            assert status == 200 and playnite_status["settings_ready"] and not playnite_status["enabled"], playnite_status
            status, persisted_catalog_entry, _ = api.post(
                "/v1/catalog/get", {"id": catalog_id}, relogged["bearer"]
            )
            assert status == 200 and persisted_catalog_entry["entry"] == catalog_entry, persisted_catalog_entry
            assert persisted_catalog_entry["revision"] == catalog_revision, persisted_catalog_entry
            if playnite_override_id is not None:
                status, persisted_provider_entry, _ = api.post(
                    "/v1/catalog/get", {"id": playnite_override_id}, relogged["bearer"]
                )
                assert status == 200, persisted_provider_entry
                assert persisted_provider_entry["entry"].get("config_overrides", {}).get("fec_percentage") == playnite_override_value, persisted_provider_entry
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
            status, reset_host, _ = api.post("/v1/nvhttp/settings_update", {
                "hostname": "Living Room & Games", "port": moved_port,
                "address_family": None, "bind_address": None,
                "output_name": "",
                "capture": None,
                "video_max_batch_size_kb": None,
                "max_bitrate": None,
                "minimum_fps_target": None,
                "audio_sink": None,
                "virtual_sink": None,
                "global_preparation": None,
                "audio_sink_capture_only": False,
                "external_ip": None,
                "wgc_pacing_smoothing": None,
                "ping_timeout_ms": None,
                "stream_audio": True,
                "expected_revision": reset_revision,
            }, relogged["bearer"])
            assert status == 200 and reset_host["ok"], reset_host
            assert reset_host["audio_sink_capture_only"] is False, reset_host
            assert reset_host["configured_address_family"] is None, reset_host
            assert reset_host["configured_bind_address"] is None, reset_host
            assert reset_host["configured_output_name"] is None, reset_host
            assert reset_host["configured_capture"] == "auto", reset_host
            assert reset_host["configured_video_max_batch_size_kb"] is None, reset_host
            assert reset_host["effective_video_max_batch_size_kb"] == 64, reset_host
            assert reset_host["video_batch_size_fallback"] is False, reset_host
            assert reset_host["configured_max_bitrate"] is None, reset_host
            assert reset_host["effective_max_bitrate_kbps"] is None, reset_host
            assert reset_host["configured_minimum_fps_target"] is None, reset_host
            assert reset_host["effective_minimum_fps_target"] == 20, reset_host
            assert reset_host["configured_audio_sink"] is None, reset_host
            assert reset_host["configured_virtual_sink"] is None, reset_host
            assert reset_host["configured_global_preparation"] == [], reset_host
            assert reset_host["configured_external_ip"] is None, reset_host
            assert isinstance(reset_host["effective_audio_sink"], str), reset_host
            assert reset_host["configured_wgc_pacing_smoothing"] is None, reset_host
            assert reset_host["effective_wgc_pacing_smoothing"] is True, reset_host
            assert reset_host["configured_ping_timeout_ms"] is None, reset_host
            assert reset_host["effective_ping_timeout_ms"] == 10_000, reset_host
            assert reset_host["stream_audio"] is True, reset_host
            assert reset_host["address_family"] == "ipv4", reset_host
            assert reset_host["bind_address"] == "127.0.0.1", reset_host
            console.eof()
        finally:
            console.close()
    print("Passed: real HTTP login/authentication, route scopes, catalog FEC/lifecycle and global-preparation exclusion CAS and restart persistence, manual Playnite Fullscreen validation, token persistence/revocation,")
    print("provider-owned Playnite override CAS/replay/inheritance and restart persistence when an imported Playnite entry is available,")
    print("Moonlight hostname/port live rebind, capture backend/output, audio/virtual-sink and structured global-preparation policy persistence,")
    print("certificate-bound FEC/audio/virtual-sink/input/Back timeout override CAS and restart persistence,")
    print("duplicate client-ID isolation and pairing admin scope enforcement,")
    print("Steam settings CAS, discovery, authorized artwork retrieval and restart persistence,")
    print("Lutris settings CAS, platform policy and restart persistence,")
    print("Playnite settings CAS, refresh policy, Windows Fullscreen entry ownership and restart persistence,")
    print("session history authorization, settings CAS, status, and restart persistence,")
    print("session restart invalidation,")
    print("input policy CAS, controller switch, gamepad profile/status, Right Alt, repeat and Back timeout settings, legacy field omission and restart persistence, configured diagnostic log filtering, strict framing, Origin/Host checks, preflight and no secret-bearing errors.")


if __name__ == "__main__":
    main()
