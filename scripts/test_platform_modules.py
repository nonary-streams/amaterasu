#!/usr/bin/env python3
"""Real-library tests for platform services, including controlled child processes."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import sys
import time

from test_component_updates import CARGO, ComponentHarness, run


class PlatformHarness(ComponentHarness):
    def package(self, sequence: int) -> Path:
        destination = self.directory / f"platform-{sequence}"
        run(self.publisher, "pack-platform", self.root, self.artifacts, destination, str(sequence))
        return destination

    def requests(self, service: str, *payloads: dict) -> list[dict]:
        commands = [
            {
                "op": "request",
                "service": service,
                "payload": payload,
                "timeout_ms": 2000,
                "operation_id": f"platform.{index}",
            }
            for index, payload in enumerate(payloads)
        ]
        responses = json.loads(run(self.host, "probe", self.root, json.dumps(commands)))
        for response in responses:
            assert response["kind"] == "result", response
        return [response["payload"] for response in responses]


def test_persistent_state(harness: PlatformHarness) -> None:
    service = "amaterasu.state/1"
    created, stale = harness.requests(
        service,
        {"action": "set", "key": "theme", "value": "dark", "expected_revision": 0},
        {"action": "set", "key": "theme", "value": "light", "expected_revision": 0},
    )
    assert created["ok"] and created["revision"] == 1, created
    assert not stale["ok"] and stale["error"]["code"] == "revision_conflict", stale

    # A new probe means a new process and new module instance, reading durable state.
    restored = harness.requests(service, {"action": "get", "key": "theme"})[0]
    assert restored["found"] and restored["value"] == "dark", restored


def test_configuration(harness: PlatformHarness) -> None:
    service = "amaterasu.configuration/1"
    fields = {
        "enabled": {"type": "boolean", "default": True},
        "fps": {"type": "integer", "default": 60, "minimum": 1, "maximum": 240},
    }
    registered, changed, rejected = harness.requests(
        service,
        {"action": "register", "fields": fields},
        {"action": "update", "values": {"fps": 120}},
        {"action": "update", "values": {"fps": 999}},
    )
    assert registered["ok"] and registered["values"]["fps"] == 60, registered
    assert changed["ok"] and changed["values"]["fps"] == 120, changed
    assert not rejected["ok"], rejected

    restored = harness.requests(service, {"action": "get"})[0]
    assert restored["ok"] and restored["values"] == {"enabled": True, "fps": 120}, restored

    # Configuration's storage belongs to its library principal, not the console.
    visible_state = harness.requests("amaterasu.state/1", {"action": "snapshot"})[0]
    assert visible_state["values"] == {"theme": "dark"}, visible_state


def test_configuration_migration_event(harness: PlatformHarness) -> None:
    fields = {
        "enabled": {"type": "boolean", "default": True},
        "frame_rate": {"type": "integer", "default": 60, "minimum": 1, "maximum": 240},
    }
    commands = [
        {"op": "subscribe", "topic": "amaterasu.configuration.changed/1", "capacity": 4},
        {
            "op": "request", "service": "amaterasu.configuration/1",
            "payload": {
                "action": "migrate", "expected_revision": 2, "fields": fields,
                "renames": {"fps": "frame_rate"},
            },
            "timeout_ms": 2000, "operation_id": "configuration.migrate",
        },
        {"op": "next_event", "subscription": 1},
    ]
    subscribed, migrated, event = json.loads(run(harness.host, "probe", harness.root, json.dumps(commands)))
    assert subscribed["kind"] == "accepted" and subscribed["id"] == 1, subscribed
    assert migrated["payload"]["ok"] and migrated["payload"]["revision"] == 3, migrated
    assert event["kind"] == "event" and event["payload"] == {"owner": "amaterasu.console", "revision": 3}, event
    restored = harness.requests("amaterasu.configuration/1", {"action": "get"})[0]
    assert restored["values"] == {"enabled": True, "frame_rate": 120}, restored


def test_session_lease_ownership(harness: PlatformHarness) -> None:
    responses = harness.requests(
        "amaterasu.session/1",
        {"action": "create", "key": "first-client"},
        {"action": "create", "key": "second-client"},
        {"action": "acquire", "session_id": 1, "resource": "display.main", "mode": "shared"},
        {"action": "acquire", "session_id": 2, "resource": "display.main", "mode": "shared"},
        {"action": "activate", "session_id": 1},
        {"action": "activate", "session_id": 2},
        {"action": "cancel", "session_id": 1},
        {"action": "snapshot", "session_id": 2},
    )
    for response in responses:
        assert response["ok"], response
    surviving_session = responses[-1]["sessions"][0]
    assert surviving_session["state"] == "active", surviving_session
    assert surviving_session["leases"] == {"display.main": "shared"}, surviving_session


def test_operation_errors_do_not_roll_back_activation(harness: PlatformHarness) -> None:
    active_before = harness.state()["active"]
    rejected = harness.requests(
        "amaterasu.configuration/1", {"action": "update", "values": {"unknown_setting": 1}}
    )[0]
    assert not rejected["ok"], rejected
    assert harness.state()["active"] == active_before


def test_diagnostics(harness: PlatformHarness) -> None:
    recorded, queried, incremented, counters = harness.requests(
        "amaterasu.diagnostics/1",
        {
            "action": "record",
            "level": "info",
            "target": "integration",
            "message": "module loaded",
            "fields": {"component": "diagnostics"},
        },
        {"action": "query", "after": 0, "limit": 4},
        {"action": "increment", "name": "requests", "amount": 2},
        {"action": "counters"},
    )
    assert recorded["ok"] and recorded["sequence"] == 1, recorded
    assert queried["records"][0]["message"] == "module loaded", queried
    assert not queried["resync_required"] and not queried["has_more"], queried
    assert incremented["value"] == 2, incremented
    assert counters["counters"] == {"requests": 2}, counters

    # Diagnostics explicitly has in-memory retention, unlike the durable catalog.
    restarted = harness.requests("amaterasu.diagnostics/1", {"action": "query"})[0]
    assert restarted["ok"] and restarted["records"] == [], restarted


def test_catalog(harness: PlatformHarness) -> None:
    service = "amaterasu.catalog/1"
    entry = {
        "name": "Integration Game",
        "tags": ["test"],
        "launch": {"kind": "executable", "program": str(Path(sys.executable).resolve())},
    }
    created, conflict, searched = harness.requests(
        service,
        {"action": "create", "entry": entry, "expected_revision": 0},
        {"action": "create", "entry": entry, "expected_revision": 0},
        {"action": "search", "query": "integration"},
    )
    assert created["ok"] and created["revision"] == 1, created
    assert not conflict["ok"] and conflict["error"]["code"] == "revision_conflict", conflict
    entry_id = created["id"]
    assert searched["entries"][0]["id"] == entry_id, searched

    restored = harness.requests(service, {"action": "get", "id": entry_id})[0]
    assert restored["ok"] and restored["entry"]["name"] == entry["name"], restored
    renamed = dict(entry, name="Renamed Game")
    updated, deleted = harness.requests(
        service,
        {"action": "update", "id": entry_id, "entry": renamed, "expected_revision": 1},
        {"action": "delete", "id": entry_id, "expected_revision": 2},
    )
    assert updated["ok"] and updated["id"] == entry_id, updated
    assert deleted["ok"] and deleted["revision"] == 3, deleted
    missing = harness.requests(service, {"action": "get", "id": entry_id})[0]
    assert not missing["ok"] and missing["error"]["code"] == "not_found", missing


def test_application_process(harness: PlatformHarness) -> None:
    launch = {
        "action": "launch",
        "operation_id": "integration.sleeper",
        "executable": str(Path(sys.executable).resolve()),
        "args": ["-c", "import time; time.sleep(30)"],
        "environment": {
            key: os.environ[key] for key in ("SYSTEMROOT", "WINDIR") if key in os.environ
        },
    }
    process = subprocess.Popen(
        [str(harness.host), "serve", str(harness.root)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    sequence = 0

    def request(payload: dict) -> dict:
        nonlocal sequence
        sequence += 1
        command = {
            "op": "request",
            "service": "amaterasu.application/1",
            "payload": payload,
            "timeout_ms": 2000,
            "operation_id": f"platform.application.{sequence}",
        }
        process.stdin.write(json.dumps(command) + "\n")
        process.stdin.flush()
        line = process.stdout.readline()
        assert line, process.stderr.read()
        response = json.loads(line)
        assert response["kind"] == "result", response
        return response["payload"]

    try:
        launched = request(launch)
        repeated = request(launch)
        conflict = request(dict(launch, args=["-c", "pass"]))
        terminated = request({"action": "terminate", "process_id": 1})
        assert launched["ok"] and launched["process"]["state"] == "running", launched
        assert launched["process"]["pid"] > 0, launched
        assert repeated["process"]["pid"] == launched["process"]["pid"], repeated
        assert not conflict["ok"] and conflict["code"] == "operation_conflict", conflict
        assert terminated["ok"] and terminated["process"]["state"] == "stopping", terminated

        deadline = time.monotonic() + 3
        while True:
            status = request({"action": "status", "process_id": 1})
            if status["process"]["state"] == "cancelled":
                break
            assert status["process"]["state"] == "stopping", status
            assert time.monotonic() < deadline, status
            time.sleep(0.02)
    finally:
        if process.stdin and not process.stdin.closed:
            process.stdin.close()
        try:
            return_code = process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            process.kill()
            return_code = process.wait(timeout=8)
        assert return_code == 0, process.stderr.read()


def configure_steam_fixture(harness: PlatformHarness) -> None:
    library = harness.directory / "steam-library"
    (library / "steamapps" / "common" / "Example").mkdir(parents=True)
    manifest = '"AppState" { "appid" "730" "name" "Example" "installdir" "Example" "StateFlags" "4" }'
    (library / "steamapps" / "appmanifest_730.acf").write_text(manifest)
    bootstrap_path = harness.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.steam"] = {"roots": [str(library)]}
    bootstrap_path.write_text(json.dumps(bootstrap))


def test_steam_disabled_policy(harness: PlatformHarness) -> None:
    responses = harness.requests(
        "amaterasu.steam/1",
        {"action": "refresh"},
        *[{"action": "status"} for _ in range(8)],
        {"action": "list"},
    )
    assert responses[0]["ok"] is False and responses[0]["code"] in {
        "settings_loading", "disabled"
    }, responses
    assert all(response["ok"] for response in responses[1:]), responses
    result = responses[-1]
    assert not result["enabled"] and not result["scanning"] and result["games"] == [], result


def test_disabled_native_devices(harness: PlatformHarness) -> None:
    # These modules must load without opening device handles. Native injection
    # and monitor creation are deliberately disabled in the signed startup policy.
    status = harness.requests("amaterasu.input/1", {"action": "status"})[0]
    assert status["ok"] and not status["enabled"] and status["lease"] is None, status
    rejected = harness.requests("amaterasu.input/1", {"action": "acquire", "ttl_ms": 1000})[0]
    assert not rejected["ok"], rejected
    status = harness.requests("amaterasu.virtual-display/1", {"action": "status"})[0]
    assert status["ok"] and not status["enabled"], status
    rejected = harness.requests("amaterasu.virtual-display/1", {
        "action": "create", "identity": "12345678-1234-4234-8234-123456789abc",
        "width": 1920, "height": 1080, "refresh_millihz": 60000, "name": "Disabled test",
    })[0]
    assert not rejected["ok"], rejected


def test_disabled_media_controls(harness: PlatformHarness) -> None:
    # Signed defaults prevent capture/encoding sessions. Capture enumeration is
    # read-only output metadata; no duplication session or encoder is opened.
    for provider in ("dxgi", "wgc"):
        before, outputs, rejected, missing, after = harness.requests(
            f"amaterasu.capture-{provider}/1",
            {"action": "status"},
            {"action": "enumerate"},
            {"action": "open", "output": "disabled-test-output", "operation_id": f"disabled.capture.{provider}"},
            {"action": "lookup", "operation_id": f"disabled.capture.{provider}"},
            {"action": "status"},
        )
        assert before["ok"] and not before["enabled"] and before["streams"] == [], before
        if before["native_supported"]:
            assert outputs["ok"] and isinstance(outputs["outputs"], list), outputs
        else:
            assert not outputs["ok"] and outputs["error"]["code"] == "unsupported_platform", outputs
        assert not rejected["ok"] and rejected["error"]["code"] == "capture_disabled", rejected
        assert not missing["ok"] and missing["error"]["code"] == "not_found", missing
        assert after == before, (before, after)

    configuration = {"width": 640, "height": 480, "fps": 30, "bitrate": 2_000_000}
    encoders, rejected, missing, status, unchanged = harness.requests(
        "amaterasu.encode/1",
        {"action": "enumerate"},
        {"action": "open", "encoder": {"clsid": "{00000000-0000-0000-0000-000000000000}",
                               "hardware_url": None, "friendly_name": "disabled-test"},
         "operation_id": "disabled.encode", "configuration": configuration},
        {"action": "lookup", "operation_id": "disabled.encode"},
        {"action": "status", "session_id": 1},
        {"action": "enumerate"},
    )
    assert encoders["ok"] and not encoders["enabled"] and encoders["encoders"] == [], encoders
    assert not rejected["ok"] and rejected["code"] == "disabled", rejected
    for result in (missing, status):
        assert not result["ok"] and result["code"] == "session_not_found", result
    assert unchanged == encoders, (encoders, unchanged)

    before, rejected, absent, closed, after = harness.requests(
        "amaterasu.video/1",
        {"action": "status"},
        {"action": "open", "output": "disabled-test-output",
         "encoder": {"clsid": "{00000000-0000-0000-0000-000000000000}",
                     "hardware_url": None, "friendly_name": "disabled-test"},
         "configuration": configuration},
        {"action": "status", "stream": 1},
        {"action": "close", "stream": 1},
        {"action": "status"},
    )
    assert before["ok"] and not before["enabled"] and before["streams"] == [], before
    assert not rejected["ok"] and rejected["code"] == "disabled", rejected
    assert absent == before, absent
    assert not closed["ok"] and closed["code"] == "not_found", closed
    assert after == before, (before, after)


def test_disabled_audio_controls(harness: PlatformHarness) -> None:
    before, endpoints, rejected, missing, after = harness.requests(
        "amaterasu.audio/1",
        {"action": "status"},
        {"action": "enumerate"},
        {"action": "open", "endpoint": "default", "operation_id": "disabled.audio"},
        {"action": "lookup", "operation_id": "disabled.audio"},
        {"action": "status"},
    )
    assert before["ok"] and not before["enabled"] and before["streams"] == [], before
    assert endpoints == {"ok": True, "enabled": False, "endpoints": []}, endpoints
    assert not rejected["ok"] and rejected["code"] == "disabled", rejected
    assert not missing["ok"] and missing["code"] == "stream_not_found", missing
    assert after == before, (before, after)


def test_disabled_moonlight_transport(harness: PlatformHarness) -> None:
    status = harness.requests("amaterasu.nvhttp/1", {"action": "status"})[0]
    assert status["ok"] and not status["enabled"], status
    assert status["http_address"] is None and status["https_address"] is None, status
    assert not status["tls_ready"] and status["connections"] == 0, status


def test_disabled_udp_transport(harness: PlatformHarness) -> None:
    control = harness.requests("amaterasu.control-transport/1", {"action": "status"})[0]
    assert control["ok"] and not control["enabled"] and control["streams"] == [], control
    status = harness.requests("amaterasu.media-transport/1", {"action": "status"})[0]
    assert status["ok"] and not status["enabled"] and status["streams"] == [], status


def test_disabled_stream_session(harness: PlatformHarness) -> None:
    status, rejected, after = harness.requests(
        "amaterasu.stream-session/1",
        {"action": "status"},
        {"action": "prepare", "peer_ip": "127.0.0.1", "video_port": 49000,
         "audio_port": 49001, "key_hex": "00" * 16, "key_id": 1,
         "connect_data": 1, "operation_id": "disabled.stream-session"},
        {"action": "status"},
    )
    assert status["ok"] and not status["enabled"] and status["sessions"] == [], status
    assert rejected == {"ok": False, "code": "disabled"}, rejected
    assert after == status, (status, after)


def test_disabled_rtsp(harness: PlatformHarness) -> None:
    status, rejected, after = harness.requests(
        "amaterasu.rtsp/1",
        {"action": "status"},
        {"action": "open", "operation_id": "disabled.rtsp", "peer_ip": "127.0.0.1",
         "video_port": 49000, "audio_port": 49001, "key_hex": "00" * 16,
         "key_id": 1, "connect_data": 1,
         "capture": {"capture_provider": "dxgi", "output": "fixture", "encoder": 0, "audio_endpoint": "default"}},
        {"action": "status"},
    )
    assert status["ok"] and not status["enabled"] and status["tickets"] == [], status
    assert status["local"] is None, status
    assert rejected == {"ok": False, "code": "disabled"}, rejected
    assert after == status, (status, after)


def test_disabled_audio_encoder(harness: PlatformHarness) -> None:
    before, encoders, rejected, missing, after = harness.requests(
        "amaterasu.audio-encode/1",
        {"action": "status"},
        {"action": "enumerate"},
        {"action": "open", "operation_id": "disabled.opus"},
        {"action": "lookup", "operation_id": "disabled.opus"},
        {"action": "status"},
    )
    assert before["ok"] and not before["enabled"] and before["streams"] == [], before
    assert encoders == {"ok": True, "enabled": False, "encoders": []}, encoders
    assert not rejected["ok"] and rejected["code"] == "disabled", rejected
    assert not missing["ok"] and missing["code"] == "stream_not_found", missing
    assert after == before, (before, after)


def main() -> None:
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-platform-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure_steam_fixture(harness)
        harness.stage(harness.package(1))
        test_persistent_state(harness)
        test_configuration(harness)
        test_configuration_migration_event(harness)
        test_session_lease_ownership(harness)
        test_operation_errors_do_not_roll_back_activation(harness)
        test_diagnostics(harness)
        test_catalog(harness)
        test_application_process(harness)
        test_steam_disabled_policy(harness)
        test_disabled_native_devices(harness)
        test_disabled_media_controls(harness)
        test_disabled_audio_controls(harness)
        test_disabled_audio_encoder(harness)
        test_disabled_moonlight_transport(harness)
        test_disabled_udp_transport(harness)
        test_disabled_stream_session(harness)
        test_disabled_rtsp(harness)
    print("Passed: persistent state, CAS conflicts, schema validation, configuration reload,")
    print("session transitions, shared lease retention, request error isolation,")
    print("catalog persistence/CRUD, diagnostic records/counters, child process lifecycle,")
    print("disabled Steam discovery, disabled native input/virtual-display policy,")
    print("and disabled DXGI/WGC/audio/Opus/encoder/video/UDP/stream-session controls without opening media sessions, plus disabled Moonlight HTTP/RTSP listeners.")


if __name__ == "__main__":
    main()
