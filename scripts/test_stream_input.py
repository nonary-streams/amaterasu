#!/usr/bin/env python3
"""Signed ENet-to-input lease integration with an in-memory input provider.

No operating-system input is injected. Native key delivery and captured-monitor
absolute mapping require separate Windows/Linux acceptance.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import selectors
import socket
import struct
import subprocess
import tempfile
import time

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from test_component_updates import CARGO, ComponentHarness, run
from test_control_transport import wait_connected
from test_host_service import Console
from test_native_media import socket_address
from test_stream_session import configuration, request, stop_client

METRICS = "test.input-probe/1"


def encrypted_message(key, sequence, kind, payload):
    plaintext = struct.pack("<HH", kind, len(payload)) + payload
    nonce = sequence.to_bytes(8, "little") + b"\0\0CC"
    encrypted = AESGCM(key).encrypt(nonce, plaintext, None)
    body = sequence.to_bytes(4, "little") + encrypted[-16:] + encrypted[:-16]
    return struct.pack("<HH", 1, len(body)) + body


def input_packet(magic, body):
    return struct.pack(">I", len(body) + 4) + struct.pack("<I", magic) + body


def keyboard(pressed, key=0x8041):
    return input_packet(3 if pressed else 4, struct.pack("<BHBH", 0, key, 0, 0))


def send_input(client, key, sequence, payload):
    packet = encrypted_message(key, sequence, 0x0206, payload)
    client.stdin.write(struct.pack(">I", len(packet)) + packet)
    client.stdin.flush()
    with selectors.DefaultSelector() as selector:
        selector.register(client.stdout, selectors.EVENT_READ)
        assert selector.select(3), "oracle did not acknowledge input submission"
        assert client.stdout.readline() == b"SENT\n"


def wait_metrics(console, predicate):
    deadline = time.monotonic() + 5
    while True:
        metrics = console.request(METRICS, {"action": "status"})
        assert metrics["ok"] and metrics["fixture"], metrics
        if predicate(metrics):
            return metrics
        assert time.monotonic() < deadline, metrics
        time.sleep(0.01)


def activate(console, session):
    start = {"action": "start", "session": session, "configuration": configuration()}
    deadline = time.monotonic() + 5
    while True:
        result = request(console, start)
        if result.get("state") == "active":
            return result
        assert result.get("code") == "control_not_authenticated", result
        assert time.monotonic() < deadline, result
        time.sleep(0.01)


def normalized_coordinate(offset, pixels):
    return (offset * 65535 + (pixels - 1) // 2) // (pixels - 1)


def check_absolute_input(console, client, key, session, expected, geometry):
    baseline = request(console, {"action": "status", "session": session})
    # Replaying ciphertext cannot create a second button-down.
    send_input(client, key, 3, input_packet(8, b"\x01"))
    if geometry:
        for sequence, coordinate in ((4, 0), (5, 15)):
            send_input(client, key, sequence, input_packet(5, struct.pack(">HHHHH", coordinate, coordinate, 0, 15, 15)))
        # Physical inclusive endpoints in the fixture's complete virtual
        # desktop, calculated independently of the Rust conversion helper.
        expected.extend([
            {"kind": "absolute", "x": 0, "y": normalized_coordinate(200, 1440)},
            {"kind": "absolute", "x": normalized_coordinate(1919, 3840), "y": normalized_coordinate(1279, 1440)},
        ])
        metrics = wait_metrics(console, lambda value: len(value["events"]) >= len(expected))
        assert metrics["events"] == expected, metrics
        return
    send_input(client, key, 4, input_packet(5, struct.pack(">HHHHH", 1, 1, 0, 15, 15)))
    deadline = time.monotonic() + 3
    while True:
        status = request(console, {"action": "status", "session": session})
        assert status["state"] == "active", status
        if status["unsupported_controls"] > baseline["unsupported_controls"]:
            break
        assert time.monotonic() < deadline, status
        time.sleep(0.01)
    assert console.request(METRICS, {"action": "status"})["events"] == expected


def end_session(console, session, geometry):
    if not geometry:
        stopped = request(console, {"action": "stop", "session": session})
        assert stopped["ok"] and stopped["state"] == "stopped", stopped
        return
    changed = console.request("test.stream-source/1", {"action": "invalidate_geometry"})
    assert changed["ok"], changed
    deadline = time.monotonic() + 5
    while True:
        status = request(console, {"action": "status", "session": session})
        if status["state"] == "failed":
            assert status["cleanup_error"] is None, status
            return
        assert time.monotonic() < deadline, status
        time.sleep(0.01)


def exercise(console, oracle, directory, geometry=False, direct=False):
    key = secrets.token_bytes(16)
    connect_data = secrets.randbits(32) or 1
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        opened = request(console, {
            "action": "prepare", "operation_id": "input-session",
            "peer_ip": "127.0.0.1", "video_port": video.getsockname()[1],
            "audio_port": audio.getsockname()[1], "key_hex": key.hex(),
            "key_id": 17, "connect_data": connect_data,
        })
        assert opened["ok"] and opened["state"] == "prepared", opened
        session = opened["session"]
        assert console.request(METRICS, {"action": "status"})["acquired"] == 0
        # A valid pre-activation key proves control authentication, but must
        # never be replayed as an input effect once activation is complete.
        initial = directory / "pre-active-key.bin"
        initial.write_bytes(encrypted_message(key, 0, 0x0206, keyboard(True, 0x8042)))
        client = subprocess.Popen([
            str(oracle), str(socket_address(opened["control_local"])[1]),
            str(connect_data), str(initial), "--interactive",
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            wait_connected(client)
            active = activate(console, session)
            assert active["absolute_input"] is geometry, active
            metrics = wait_metrics(console, lambda value: value["active"])
            assert metrics["acquired"] == 1 and metrics["events"] == [], metrics
            # Wire vectors independently encode a key, motion and a held button.
            send_input(client, key, 1, keyboard(True))
            send_input(client, key, 2, input_packet(7, struct.pack(">hh", -300, 700)))
            send_input(client, key, 3, input_packet(8, b"\x01"))
            expected = [
                {"kind": "key", "key": {"platform": "moonlight_vk", "code": 65}, "state": "down"},
                {"kind": "relative", "dx": -300, "dy": 700},
                {"kind": "button", "button": "left", "pressed": True},
            ]
            metrics = wait_metrics(console, lambda value: len(value["events"]) >= 3)
            assert metrics["events"] == expected, metrics
            # The same events in the same order, whichever transport carried them.
            assert (metrics["direct_batches"] > 0) is direct, metrics
            check_absolute_input(console, client, key, session, expected, geometry)
            # The one-second lease must renew while keys remain held.
            metrics = wait_metrics(console, lambda value: value["renewed"] >= 2)
            assert metrics["active"] and metrics["expired"] == 0, metrics
            end_session(console, session, geometry)
            metrics = wait_metrics(console, lambda value: not value["active"])
            assert metrics["released"] == 1 and metrics["expired"] == 0, metrics
            assert metrics["held_keys"] == 0 and metrics["held_buttons"] == 0, metrics
            assert not metrics["events_truncated"], metrics
        finally:
            stop_client(client)


def run_case(oracle, geometry, direct=False):
    with tempfile.TemporaryDirectory(prefix="amaterasu-stream-input-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        settings = bootstrap["policy"]["settings"]
        settings["amaterasu.audio-encode"] = {"enabled": True}
        settings["amaterasu.input-probe"] = {"enabled": True, "direct_batches": direct}
        settings["amaterasu.stream-source"] = {"desktop_geometry": geometry}
        settings["amaterasu.stream-session"] = {
            "enabled": True, "allowed_principals": ["amaterasu.console"],
            "enable_input": True, "input_lease_ttl_ms": 1000,
        }
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {
                "enabled": True, "allowed_principals": ["amaterasu.stream-session"],
            }
        path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            exercise(console, oracle, directory, geometry, direct)
            console.eof()
        finally:
            console.close()


def main():
    setting = os.environ.get("AMATERASU_ENET_CLIENT_ORACLE")
    if not setting:
        raise SystemExit("Set AMATERASU_ENET_CLIENT_ORACLE to the rebuilt interactive C oracle")
    oracle = Path(setting).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    for geometry in (False, True):
        run_case(oracle, geometry)
    # The unchanged session against a provider that serves batches directly.
    run_case(oracle, False, direct=True)
    print("Passed: signed authenticated input dispatch through queued and direct batches, pre-active/replay rejection, captured-monitor absolute mapping, missing-geometry rejection, lease renewal and held-input cleanup after geometry failure. In-memory fixtures do not qualify native OS delivery.")


if __name__ == "__main__":
    main()
