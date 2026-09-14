#!/usr/bin/env python3
"""Signed control module against primary ENet C and independent Python AES-GCM."""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import selectors
import subprocess
import tempfile
import time

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console


def encrypt_control(key: bytes, kind: int, payload: bytes) -> bytes:
    plaintext = kind.to_bytes(2, "little") + len(payload).to_bytes(2, "little") + payload
    nonce = bytes(10) + b"CC"
    ciphertext_and_tag = AESGCM(key).encrypt(nonce, plaintext, None)
    ciphertext, tag = ciphertext_and_tag[:-16], ciphertext_and_tag[-16:]
    length = 4 + len(tag) + len(ciphertext)
    return b"\x01\x00" + length.to_bytes(2, "little") + bytes(4) + tag + ciphertext


def decrypt_control(key: bytes, envelope: bytes) -> tuple[int, bytes]:
    assert len(envelope) >= 28 and envelope[:2] == b"\x01\x00"
    assert int.from_bytes(envelope[2:4], "little") == len(envelope) - 4
    assert envelope[4:8] == bytes(4), "first host sequence must be zero"
    plaintext = AESGCM(key).decrypt(bytes(10) + b"HC", envelope[24:] + envelope[8:24], None)
    kind = int.from_bytes(plaintext[:2], "little")
    assert int.from_bytes(plaintext[2:4], "little") == len(plaintext) - 4
    return kind, plaintext[4:]


def wait_connected(client: subprocess.Popen) -> None:
    with selectors.DefaultSelector() as selector:
        selector.register(client.stdout, selectors.EVENT_READ)
        assert selector.select(3), "C client did not report connection"
        line = client.stdout.readline()
        assert line == b"CONNECTED\n", (line, client.poll())


def echo_once(console: Console, stream: int) -> dict:
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        response = console.request("test.media-probe/1", {"action": "control_echo", "stream": stream})
        assert response["ok"], response
        if not response["pending"]:
            return response
        time.sleep(0.005)
    raise AssertionError("authenticated control message was not delivered")


def round_trip(console: Console, oracle: Path, directory: Path, length: int) -> None:
    key = secrets.token_bytes(16)
    connect_data = secrets.randbits(32)
    kind = 0x1234
    payload = bytes(index % 251 for index in range(length))
    opened = console.request("test.media-probe/1", {
        "action": "control_open",
        "key_hex": key.hex(),
        "connect_data": connect_data,
    })
    assert opened["ok"] and not opened["authenticated"], opened
    assert opened["state"] == "listening", opened
    stream = opened["stream"]
    host, port = opened["local"].rsplit(":", 1)
    assert host == "127.0.0.1", opened
    payload_path = directory / f"control-{length}.bin"
    payload_path.write_bytes(encrypt_control(key, kind, payload))
    client = subprocess.Popen(
        [str(oracle), port, str(connect_data), str(payload_path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        wait_connected(client)
        echoed = echo_once(console, stream)
        assert echoed["retry_preserved"] and echoed["bytes"] == length, echoed
        assert (echoed["message_type"], echoed["channel"], echoed["sequence"]) == (kind, 2, 0), echoed
        output, errors = client.communicate(timeout=5)
        assert client.returncode == 0, errors.decode(errors="replace")
        assert output.startswith(b"RECEIVED "), output[:100]
        reply = bytes.fromhex(output.removeprefix(b"RECEIVED ").decode().strip())
        assert decrypt_control(key, reply) == (kind, payload)
    finally:
        if client.poll() is None:
            client.kill()
            client.communicate(timeout=2)
        closed = console.request("test.media-probe/1", {"action": "control_close", "stream": stream})
        assert closed["ok"], closed


def main() -> None:
    oracle_setting = os.environ.get("AMATERASU_ENET_CLIENT_ORACLE")
    if not oracle_setting:
        raise SystemExit("Set AMATERASU_ENET_CLIENT_ORACLE using scripts/control_enet_oracle/build.sh")
    oracle = Path(oracle_setting).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-control-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.control-transport"] = {
            "enabled": True,
            "allow_remote": False,
            "allowed_principals": ["amaterasu.media-probe"],
        }
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-media-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            for length in (0, 2000, 65511):
                round_trip(console, oracle, directory, length)
            console.eof()
        finally:
            console.close()
    print("Passed: signed control transport, primary C ENet handshake/fragmentation, independent GCM, native sink retry, and exact 0/2000/65511-byte round trips.")


if __name__ == "__main__":
    main()
