#!/usr/bin/env python3
"""Signed native interfaces and software Opus encoding; no hardware streams opened."""
from __future__ import annotations

import tempfile
import json
import secrets
import socket
import struct

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.exceptions import InvalidTag
from pathlib import Path
from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console


def multiply(left, right):
    """GF(256) polynomial arithmetic, independently checking the wire parity."""
    product = 0
    while right:
        if right & 1:
            product ^= left
        left <<= 1
        if left & 256:
            left ^= 0x11D
        right >>= 1
    return product


def inverse(value):
    # Find the multiplicative inverse independently of the Rust exponent method.
    return next(candidate for candidate in range(1, 256) if multiply(value, candidate) == 1)


def parity_row(coefficients, shards):
    result = bytearray(len(shards[0]))
    for coefficient, shard in zip(coefficients, shards, strict=True):
        assert len(shard) == len(result)
        for index, byte in enumerate(shard):
            result[index] ^= multiply(coefficient, byte)
    return bytes(result)


def receive_datagrams(receiver, count, expected_source):
    packets = []
    receiver.settimeout(3)
    for _ in range(count):
        packet, source = receiver.recvfrom(65535)
        assert source == expected_source, (source, expected_source)
        packets.append(packet)
    receiver.settimeout(0.05)
    try:
        extra = receiver.recvfrom(65535)
    except socket.timeout:
        return packets
    raise AssertionError(f"unexpected extra UDP datagram: {extra!r}")


def socket_address(value):
    host, port = value.rsplit(":", 1)
    return host, int(port)


def verify_video(datagrams, key, expected):
    packets = []
    for index, datagram in enumerate(datagrams):
        if key is not None:
            assert datagram[:8] == index.to_bytes(8, "little"), "GCM nonce sequence"
            assert datagram[8:12] == b"\0\0\0V"
            assert datagram[12:16] == (1).to_bytes(4, "little")
            payload = datagram[32:] + datagram[16:32]
            packet = AESGCM(key).decrypt(datagram[:12], payload, None)
            damaged = bytearray(payload)
            damaged[-1] ^= 1
            try:
                AESGCM(key).decrypt(datagram[:12], bytes(damaged), None)
            except InvalidTag:
                pass
            else:
                raise AssertionError("tampered video GCM tag was accepted")
        else:
            packet = datagram
        assert len(packet) == 1040
        assert packet[:2] == b"\x90\0"
        sequence, timestamp, ssrc = struct.unpack("!HII", packet[2:12])
        assert (sequence, timestamp, ssrc) == (index, 90000, 0)
        assert packet[12:16] == bytes(4)
        assert int.from_bytes(packet[20:24], "little") == 1
        assert packet[27] == 0
        fec = int.from_bytes(packet[28:32], "little")
        assert fec >> 22 == 2 and (fec >> 12) & 1023 == index
        percentage = (fec >> 4) & 255
        assert (2 * percentage + 99) // 100 == 2
        packets.append(packet)
    assert packets[0][24] == 5 and packets[1][24] == 3
    frame = packets[0][32:] + packets[1][32:]
    assert frame[:4] == b"\x01\0\0\x02"
    assert frame[8:8 + len(expected)] == expected
    assert not any(frame[8 + len(expected):]), "nonzero FEC padding"
    assert int.from_bytes(frame[4:6], "little") == 700
    data = []
    for packet in packets[:2]:
        shard = bytearray(packet)
        shard[:16] = bytes(16)
        shard[28:32] = bytes(4)
        data.append(shard)
    for index, packet in enumerate(packets[2:]):
        parity = parity_row([inverse(2 ^ index), inverse(3 ^ index)], data)
        # These are the parity fields not overwritten with transport metadata.
        assert packet[16:20] == parity[16:20]
        assert packet[24:27] == parity[24:27]
        assert packet[32:] == parity[32:]


def verify_audio(packets, key, key_id, expected):
    data = []
    for index, packet in enumerate(packets[:4]):
        assert packet[:2] == b"\x80\x61"
        assert struct.unpack("!HII", packet[2:12]) == (index, index * 10, 0)
        payload = packet[12:]
        data.append(payload)
        if key is not None:
            assert len(payload) == 176
            iv = ((key_id + index) & 0xffffffff).to_bytes(4, "big") + bytes(12)
            decryptor = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
            plaintext = decryptor.update(payload) + decryptor.finalize()
            padding = plaintext[-1]
            assert 1 <= padding <= 16 and plaintext[-padding:] == bytes([padding]) * padding
            payload = plaintext[:-padding]
        assert payload == expected[index]
        assert len(payload) == 160, "actual Opus CBR size changed"
    matrix = [(0x77, 0x40, 0x38, 0x0E), (0xC7, 0xA7, 0x0D, 0x6C)]
    for index, packet in enumerate(packets[4:]):
        assert packet[:2] == b"\x80\x7f"
        assert struct.unpack("!HII", packet[2:12]) == (4 + index, 0, 0)
        assert packet[12:14] == bytes([index, 97])
        assert struct.unpack("!HII", packet[14:24]) == (0, 0, 0)
        assert packet[24:] == parity_row(matrix[index], data)


def test_udp_transport(console, encrypted):
    key = secrets.token_bytes(16) if encrypted else None
    key_id = secrets.randbits(32)
    with (
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video,
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio,
    ):
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        settings = {
            "action": "transport_udp",
            "video_port": video.getsockname()[1],
            "audio_port": audio.getsockname()[1],
            "encrypted": encrypted,
            "key_id": key_id,
            "key_hex": key.hex() if key else "",
        }
        console.sequence += 1
        console.send({
            "op": "request",
            "service": "test.media-probe/1",
            "payload": settings,
            "timeout_ms": 5000,
            "operation_id": f"native-udp-{console.sequence}",
        })
        response = console.receive()
        assert response["kind"] == "result", response
        result = response["payload"]
        assert result["ok"], result
        status = result["transport_status"]
        assert status["state"] == "drained" and status["queued_datagrams"] == 0, status
        assert status["sent_datagrams"] == 10 and status["discarded_datagrams"] == 0, status
        video_packets = receive_datagrams(video, 4, socket_address(result["video_local"]))
        audio_packets = receive_datagrams(audio, 6, socket_address(result["audio_local"]))
        verify_video(video_packets, key, bytes.fromhex(result["expected_video_hex"]))
        verify_audio(
            audio_packets,
            key,
            key_id,
            [bytes.fromhex(value) for value in result["expected_audio_hex"]],
        )


def main():
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-native-media-") as temporary:
        harness = ComponentHarness(Path(temporary))
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.audio-encode"] = {"enabled": True}
        bootstrap["policy"]["settings"]["amaterasu.media-transport"] = {
            "enabled": True,
            "allow_remote": False,
            "allowed_principals": ["amaterasu.media-probe"],
        }
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = Path(temporary) / "media-probe"
        run(harness.publisher, "pack-media-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            for command in [
                {"op": "native_register", "service": "amaterasu.capture-dxgi/1", "factory": 1},
                {
                    "op": "native_bind",
                    "service": "amaterasu.capture-dxgi/1",
                    "interface_major": 1,
                    "min_minor": 0,
                    "min_size": 40,
                },
                {"op": "native_release", "binding": 1},
            ]:
                console.send(command)
                reply = console.receive()
                assert reply["kind"] == "error", reply
                if command["op"] != "native_release":
                    assert reply["code"] == "permission_denied", reply
                assert "address" not in reply and "table" not in reply, reply
            for _ in range(32):
                result = console.request("test.media-probe/1", {})
                assert result["ok"] and result["callbacks"] == 0, result
                for provider in (
                    "capture", "encoder", "video", "wgc", "audio", "audio_encoder",
                    "capture_geometry", "wgc_geometry",
                ):
                    assert result[f"{provider}_status"] == 12, result
            encoded = console.request("test.media-probe/1", {"action": "encode_opus"})
            assert encoded["ok"] and encoded["packets"] == 2, encoded
            assert encoded["decoded_frames"] == 960 and encoded["encoded_bytes"] > 0, encoded
            assert encoded["retry_preserved"] and encoded["eos_once"], encoded
            test_udp_transport(console, encrypted=False)
            test_udp_transport(console, encrypted=True)
            console.eof()
        finally:
            console.close()
    print(
        "Passed: signed media native binding, absent-stream isolation, denied console "
        "native access, actual Opus PCM encoding, and native loopback UDP video/audio "
        "with independently checked FEC, AES-GCM/CBC, and drained queues; no hardware opened."
    )


if __name__ == "__main__":
    main()
