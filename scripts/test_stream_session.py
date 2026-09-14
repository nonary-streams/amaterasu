#!/usr/bin/env python3
"""Signed session orchestration with synthetic capture and real Opus/ENet/UDP.

The video bytes are a framing fixture, not a decodable H.264 stream. This test
makes no claim about hardware capture, RTSP negotiation or Moonlight playback.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import socket
import struct
import subprocess
import tempfile
import time

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from test_component_updates import CARGO, ComponentHarness, run
from test_control_transport import encrypt_control, wait_connected
from test_host_service import Console
from test_native_media import inverse, parity_row, socket_address

SERVICE = "amaterasu.stream-session/1"
SOURCE = "test.stream-source/1"


def request(console, payload):
    console.sequence += 1
    console.send({"op": "request", "service": SERVICE, "payload": payload,
                  "timeout_ms": 10000, "operation_id": f"stream-test.{console.sequence}"})
    result = console.receive()
    assert result["kind"] == "result", result
    return result["payload"]


def configuration(endpoint="default"):
    return {"capture_provider": "dxgi", "output": "synthetic-display", "encoder": 0,
            "configuration": {"width": 16, "height": 16, "fps": 30, "bitrate": 1000000},
            "audio_endpoint": endpoint}


def collect(receiver, expected_source):
    packets = []
    receiver.settimeout(0.05)
    while True:
        try:
            packet, source = receiver.recvfrom(65535)
        except socket.timeout:
            return packets
        assert source == expected_source, (source, expected_source)
        packets.append(packet)


def verify_video(datagrams, key, expected_frames):
    assert len(datagrams) == 32, len(datagrams)
    frames = {}
    for sequence, datagram in enumerate(datagrams):
        assert datagram[:12] == sequence.to_bytes(8, "little") + b"\0\0\0V"
        packet = AESGCM(key).decrypt(datagram[:12], datagram[32:] + datagram[16:32], None)
        assert packet[:2] == b"\x90\0" and len(packet) == 1040
        assert int.from_bytes(packet[2:4], "big") == sequence
        frame = int.from_bytes(packet[20:24], "little")
        assert int.from_bytes(datagram[12:16], "little") == frame
        fec = int.from_bytes(packet[28:32], "little")
        index, data_count = (fec >> 12) & 1023, fec >> 22
        assert data_count == 2 and index < 4 and frame == sequence // 4 + 1
        assert int.from_bytes(packet[4:8], "big") == (frame - 1) * 900
        frames.setdefault(frame, {})[index] = packet
    assert sorted(frames) == list(range(1, 9)), sorted(frames)
    for frame, parts in frames.items():
        assert sorted(parts) == list(range(4)), parts.keys()
        payload = parts[0][32:] + parts[1][32:]
        shards = []
        for index in (0, 1):
            shard = bytearray(parts[index])
            shard[:16] = bytes(16)
            shard[28:32] = bytes(4)
            shards.append(shard)
        for index in (0, 1):
            parity = parity_row([inverse(2 ^ index), inverse(3 ^ index)], shards)
            packet = parts[index + 2]
            assert packet[16:20] == parity[16:20]
            assert packet[24:27] == parity[24:27]
            assert packet[32:] == parity[32:]
        expected = expected_frames(frame - 1)
        assert payload[8:8 + len(expected)] == expected, frame
        assert not any(payload[8 + len(expected):]), "nonzero frame padding"


def verify_audio(datagrams, key, key_id):
    data = {}
    parity = []
    for packet in datagrams:
        if packet[:2] == b"\x80\x61":
            sequence, timestamp, ssrc = struct.unpack("!HII", packet[2:12])
            assert timestamp == sequence * 10 and ssrc == 0
            ciphertext = packet[12:]
            assert len(ciphertext) == 176
            iv = ((key_id + sequence) & 0xffffffff).to_bytes(4, "big") + bytes(12)
            decryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
            padded = decryptor.update(ciphertext) + decryptor.finalize()
            padding = padded[-1]
            assert padding == 16 and padded[-padding:] == bytes([padding]) * padding
            assert len(padded[:-padding]) == 160
            assert sequence not in data
            data[sequence] = ciphertext
        else:
            assert packet[:2] == b"\x80\x7f", packet[:2]
            parity.append(packet)
    # Eight captured frames plus the encoder's lookahead drain packet.
    assert sorted(data) == list(range(9)), sorted(data)
    assert len(parity) == 4, len(parity)
    matrix = [(0x77, 0x40, 0x38, 0x0E), (0xC7, 0xA7, 0x0D, 0x6C)]
    for packet in parity:
        shard, payload_type = packet[12:14]
        base, timestamp, ssrc = struct.unpack("!HII", packet[14:24])
        assert shard in (0, 1) and payload_type == 97 and base in (0, 4)
        assert timestamp == base * 10 and ssrc == 0
        assert packet[24:] == parity_row(matrix[shard], [data[base + offset] for offset in range(4)])


def start_client(oracle, directory, opened, key, connect_data):
    payload = directory / "idr.bin"
    payload.write_bytes(encrypt_control(key, 0x0302, bytes(2)))
    port = str(socket_address(opened["control_local"])[1])
    client = subprocess.Popen([str(oracle), port, str(connect_data), str(payload)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        wait_connected(client)
    except BaseException:
        stop_client(client)
        raise
    return client


def stop_client(client):
    if client.poll() is None:
        client.kill()
    client.communicate(timeout=2)


def exercise_session(console, oracle, directory):
    key, key_id, connect_data = secrets.token_bytes(16), secrets.randbits(32), secrets.randbits(32)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        prepare = {"action": "prepare", "operation_id": "synthetic-session", "peer_ip": "127.0.0.1",
                   "video_port": video.getsockname()[1], "audio_port": audio.getsockname()[1],
                   "key_hex": key.hex(), "key_id": key_id, "connect_data": connect_data}
        opened = request(console, prepare)
        assert opened["ok"] and opened["state"] == "prepared", opened
        assert opened["packet_bytes"] == 160 and opened["wire_audio_bytes"] == 176, opened
        session = opened["session"]
        assert request(console, prepare)["session"] == session
        conflict = request(console, dict(prepare, key_id=key_id ^ 1))
        assert conflict["code"] == "operation_conflict", conflict
        start = {"action": "start", "session": session, "configuration": configuration()}
        premature = request(console, start)
        assert premature["code"] == "control_not_authenticated", premature
        assert request(console, {"action": "status", "session": session})["state"] == "prepared"
        client = start_client(oracle, directory, opened, key, connect_data)
        try:
            deadline = time.monotonic() + 3
            while True:
                active = request(console, start)
                if active.get("state") == "active":
                    break
                assert active.get("code") == "control_not_authenticated", active
                assert time.monotonic() < deadline, active
                time.sleep(0.005)
            while True:
                metrics = console.request(SOURCE, {"action": "status"})
                if metrics["video"]["frames"] == 8 and metrics["audio"]["frames"] == 8 and metrics["video"]["force_idr"]:
                    break
                assert time.monotonic() < deadline, metrics
                time.sleep(0.005)
            stopped = request(console, {"action": "stop", "session": session})
            assert stopped["ok"] and stopped["state"] == "stopped", stopped
            assert stopped["error"] is None and stopped["cleanup_error"] is None, stopped
            assert stopped["control_messages"] == 1 and stopped["rejected_controls"] == 0, stopped
            assert request(console, {"action": "stop", "session": session}) == stopped
            assert request(console, {"action": "lookup", "operation_id": prepare["operation_id"]}) == stopped
            metrics = console.request(SOURCE, {"action": "status"})
            for kind in ("video", "audio"):
                assert metrics[kind]["active"] == 0 and metrics[kind]["opened"] == metrics[kind]["closed"] == 1, metrics
            video_packets = collect(video, socket_address(opened["video_local"]))
            audio_packets = collect(audio, socket_address(opened["audio_local"]))
            verify_video(video_packets, key, expected_video)
            verify_audio(audio_packets, key, key_id)
        finally:
            stop_client(client)


def exercise_format_rollback(console, oracle, directory):
    key, connect_data = secrets.token_bytes(16), secrets.randbits(32)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        opened = request(console, {
            "action": "prepare", "operation_id": "unsupported-format", "peer_ip": "127.0.0.1",
            "video_port": video.getsockname()[1], "audio_port": audio.getsockname()[1],
            "key_hex": key.hex(), "key_id": 17, "connect_data": connect_data,
        })
        assert opened["state"] == "prepared", opened
        client = start_client(oracle, directory, opened, key, connect_data)
        try:
            deadline = time.monotonic() + 3
            while True:
                failed = request(console, {"action": "start", "session": opened["session"],
                                           "configuration": configuration("synthetic-unsupported-format")})
                if failed.get("state") == "failed":
                    break
                assert failed.get("code") == "control_not_authenticated", failed
                assert time.monotonic() < deadline, failed
                time.sleep(0.005)
            assert failed["error"] == "unsupported_audio_format" and failed["cleanup_error"] is None, failed
            assert request(console, {"action": "stop", "session": opened["session"]}) == failed
            metrics = console.request(SOURCE, {"action": "status"})
            for kind in ("video", "audio"):
                assert metrics[kind]["active"] == 0 and metrics[kind]["opened"] == metrics[kind]["closed"] == 2, metrics
                assert metrics[kind]["frames"] == 8, metrics
            assert collect(video, socket_address(opened["video_local"])) == []
            assert collect(audio, socket_address(opened["audio_local"])) == []
        finally:
            stop_client(client)


def expected_video(sequence):
    # Independently generated fixture contract; keep this separate from provider output.
    return b"\0\0\0\x01\x65" + bytes((index * 17 + sequence * 13) % 251 + 1 for index in range(5, 1700))


def main():
    setting = os.environ.get("AMATERASU_ENET_CLIENT_ORACLE")
    if not setting:
        raise SystemExit("Set AMATERASU_ENET_CLIENT_ORACLE using scripts/control_enet_oracle/build.sh")
    oracle = Path(setting).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-stream-session-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        settings = bootstrap["policy"]["settings"]
        settings["amaterasu.audio-encode"] = {"enabled": True}
        settings["amaterasu.stream-session"] = {"enabled": True, "allowed_principals": ["amaterasu.console"]}
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {"enabled": True, "allowed_principals": ["amaterasu.stream-session"]}
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            exercise_session(console, oracle, directory)
            exercise_format_rollback(console, oracle, directory)
            input_status = console.request("test.input-probe/1", {"action": "status"})
            assert input_status["enabled"] is False, input_status
            assert input_status["acquired"] == 0 and input_status["events"] == [], input_status
            console.eof()
        finally:
            console.close()
    print("Passed: signed stream-session prepare/authenticate/start/drain/stop, synthetic fragmented video and PCM, real Opus and encrypted UDP, C ENet control IDR, idempotency, provider cleanup and post-open format-failure rollback. Hardware and full client playback remain unqualified.")


if __name__ == "__main__":
    main()
