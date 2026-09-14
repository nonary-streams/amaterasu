#!/usr/bin/env python3
"""Signed encrypted RTSP, armed sessions, synthetic capture and real media transport.

The trusted ticket API authorizes either fixed ports or token-based UDP discovery.
This does not qualify NVHTTP launch, hardware capture or a Moonlight player.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import socket
import struct
import tempfile
import time

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from test_component_updates import CARGO, ComponentHarness, WORKSPACE, run
from test_host_service import Console
from test_native_media import socket_address
from test_stream_session import collect, expected_video, start_client, stop_client, verify_audio, verify_video

SERVICE = "amaterasu.rtsp/1"
SOURCE = "test.stream-source/1"


def request(console, payload):
    console.sequence += 1
    console.send({"op": "request", "service": SERVICE, "payload": payload,
                  "timeout_ms": 10000, "operation_id": f"rtsp-test.{console.sequence}"})
    result = console.receive()
    assert result["kind"] == "result", result
    return result["payload"]


class RtspClient:
    def __init__(self, address, url, key):
        self.address = socket_address(address)
        self.url = url
        self.key = key
        self.sequence = 0
        self.response_sequence = 0
        self.session = None
        self.last_envelope = None

    def exchange(self, envelope):
        with socket.create_connection(self.address, timeout=3) as connection:
            connection.sendall(envelope)
            response = bytearray()
            while True:
                try:
                    part = connection.recv(65536)
                except ConnectionResetError:
                    return bytes(response)
                if not part:
                    return bytes(response)
                response.extend(part)
                assert len(response) <= 100000, "unbounded RTSP response"

    def transact(self, method, target, headers=None, body=b"", expected=200):
        self.sequence += 1
        fields = {"CSeq": str(self.sequence), "Content-Length": str(len(body))}
        if self.session:
            fields["Session"] = self.session
        fields.update(headers or {})
        lines = [f"{method} {target} RTSP/1.0"] + [f"{key}: {value}" for key, value in fields.items()]
        plaintext = ("\r\n".join(lines) + "\r\n\r\n").encode() + body
        nonce = self.sequence.to_bytes(4, "little") + bytes(6) + b"CR"
        sealed = AESGCM(self.key).encrypt(nonce, plaintext, None)
        envelope = struct.pack("!II", 0x80000000 | len(plaintext), self.sequence) + sealed[-16:] + sealed[:-16]
        self.last_envelope = envelope
        response = self.exchange(envelope)
        assert len(response) >= 24, response
        length, sequence = struct.unpack("!II", response[:8])
        assert length == 0x80000000 | (len(response) - 24)
        self.response_sequence += 1
        assert sequence == self.response_sequence, "response nonce reset across TCP connections"
        nonce = sequence.to_bytes(4, "little") + bytes(6) + b"HR"
        plaintext = AESGCM(self.key).decrypt(nonce, response[24:] + response[8:24], None)
        header, payload = plaintext.split(b"\r\n\r\n", 1)
        lines = header.decode().split("\r\n")
        assert int(lines[0].split()[1]) == expected, plaintext
        fields = {}
        for line in lines[1:]:
            name, value = line.split(": ", 1)
            assert name.lower() not in fields, "duplicate response header"
            fields[name.lower()] = value
        assert fields["cseq"] == str(self.sequence)
        assert int(fields["content-length"]) == len(payload)
        return fields, payload

    def setup(self, on_setup=None):
        self.transact("OPTIONS", self.url)
        assert self.exchange(self.last_envelope) == b"", "cross-connection replay accepted"
        _, description = self.transact("DESCRIBE", self.url, {"Accept": "application/sdp"})
        assert b"H264/90000" in description and b"opus/48000/2" in description
        ports = {}
        for name, target in (("audio", "audio/0/0"), ("video", "video/0/0"), ("control", "control/13/0")):
            fields, _ = self.transact("SETUP", f"streamid={target}", {"Transport": "unicast;X-GS-ClientPort=50000-50001"})
            self.session = fields["session"].split(";", 1)[0]
            ports[name] = int(fields["transport"].split("server_port=", 1)[1].split(";", 1)[0].split("-", 1)[0])
            if name == "control":
                ports["connect_data"] = int(fields["x-ss-connect-data"])
            if on_setup:
                on_setup(name, ports[name], fields)
        return ports


def announcement():
    # Primary client fixture with explicit substitutions for this prepared profile.
    text = (WORKSPACE / "crates/gamestream/src/sdp/fixtures/moonlight-h264.sdp").read_text()
    replacements = {"encryptionEnabled:0": "encryptionEnabled:7", "clientViewportWd:1920": "clientViewportWd:16",
                    "clientViewportHt:1080": "clientViewportHt:16", "maxFPS:60": "maxFPS:30",
                    "packetSize:1392": "packetSize:1024", "BitrateKbps:16000": "BitrateKbps:1000",
                    "configuredBitrateKbps:20000": "configuredBitrateKbps:1000",
                    "clientRefreshRateX100:6000": "clientRefreshRateX100:3000", "packetDuration:5": "packetDuration:10"}
    for before, after in replacements.items():
        text = text.replace(before, after)
    return text.replace("\n", "\r\n").encode()


def ticket_config(video, audio, key, name):
    return {"action": "open", "operation_id": name, "peer_ip": "127.0.0.1",
            "video_port": video.getsockname()[1], "audio_port": audio.getsockname()[1],
            "key_hex": key.hex(), "key_id": 17, "connect_data": 4321,
            "capture": {"capture_provider": "dxgi", "output": "synthetic-display", "encoder": 0, "audio_endpoint": "default"},
            "profile": {"width": 16, "height": 16, "fps": 30, "bitrate_kbps": 1000}}


class PeerDiscovery:
    """Independent implementation of Moonlight's payload[16] + BE32 ping."""

    def __init__(self, video, audio):
        self.sockets = {"video": video, "audio": audio}
        self.tokens = {}
        self.destinations = {}
        self.decoy = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.decoy.bind(("127.0.0.1", 0))

    def on_setup(self, medium, port, fields):
        if medium == "control":
            assert "x-ss-ping-payload" not in fields
            return
        token = fields["x-ss-ping-payload"].encode("ascii")
        assert len(token) == 16 and token not in self.tokens.values()
        self.tokens[medium] = token
        self.destinations[medium] = ("127.0.0.1", port)
        if medium == "audio":
            self.send("audio")

    def send(self, medium):
        self.sockets[medium].sendto(self.tokens[medium] + struct.pack("!I", 1),
                                    self.destinations[medium])

    def finish(self, console, baseline):
        destination = self.destinations["video"]
        # Legacy, truncated, oversized and other-lane tokens cannot select a port.
        for packet in (b"PING", self.tokens["video"],
                       self.tokens["video"] + bytes(5),
                       self.tokens["audio"] + struct.pack("!I", 1)):
            self.decoy.sendto(packet, destination)
        # Even knowledge of the established audio token cannot rebind that lane.
        self.decoy.sendto(self.tokens["audio"] + struct.pack("!I", 2),
                          self.destinations["audio"])
        deadline = time.monotonic() + 0.2
        while time.monotonic() < deadline:
            metrics = console.request(SOURCE, {"action": "status"})
            for medium in ("video", "audio"):
                assert metrics[medium]["opened"] == baseline[medium]["opened"], metrics
            time.sleep(0.005)
        # Video starts after PLAY/control; only now may both capture providers open.
        self.send("video")

    def close(self):
        try:
            assert collect(self.decoy, self.destinations["video"]) == [], "media reached an unselected port"
        finally:
            self.decoy.close()


def exercise(console, oracle, directory, discovery=False):
    key = secrets.token_bytes(16)
    baseline = console.request(SOURCE, {"action": "status"})
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        config = ticket_config(video, audio, key, f"encrypted-rtsp-{discovery}")
        association = PeerDiscovery(video, audio) if discovery else None
        if association:
            config.update(discover_peers=True, video_port=0, audio_port=0)
        opened = request(console, config)
        assert opened["ok"] and opened["state"] == "ready", opened
        assert request(console, config)["ticket"] == opened["ticket"]
        assert request(console, dict(config, key_id=18))["code"] == "operation_conflict"
        client = RtspClient(opened["local"], opened["rtsp_url"], key)
        assert client.exchange(b"OPTIONS / RTSP/1.0\r\nCSeq: 1\r\n\r\n") == b""
        ports = client.setup(association.on_setup if association else None)
        assert ports["connect_data"] == config["connect_data"]
        client.transact("ANNOUNCE", "streamid=control/13/0", {"Content-Type": "application/sdp"}, announcement())
        client.transact("PLAY", "/")
        armed = request(console, {"action": "status", "ticket": opened["ticket"]})
        assert armed["state"] == "playing" and armed["stream_state"] == "armed", armed
        if association:
            public_status = json.dumps(armed)
            for token in association.tokens.values():
                assert token.decode() not in public_status, "status exposed a discovery token"
        metrics = console.request(SOURCE, {"action": "status"})
        for kind in ("video", "audio"):
            assert metrics[kind]["opened"] == baseline[kind]["opened"], metrics
        # Control starts after PLAY, matching the real client's connection order.
        control = start_client(oracle, directory, {"control_local": f"127.0.0.1:{ports['control']}"}, key, ports["connect_data"])
        try:
            if association:
                association.finish(console, baseline)
            deadline = time.monotonic() + 3
            while True:
                metrics = console.request(SOURCE, {"action": "status"})
                complete_frames = all(metrics[kind]["frames"] == baseline[kind]["frames"] + 8
                                      for kind in ("video", "audio"))
                new_idr = metrics["video"]["force_idr"] > baseline["video"]["force_idr"]
                if complete_frames and new_idr:
                    break
                assert time.monotonic() < deadline, (metrics, request(console, {"action": "status", "ticket": opened["ticket"]}))
                time.sleep(0.005)
            client.transact("TEARDOWN", "/")
            closed = request(console, {"action": "close", "ticket": opened["ticket"]})
            assert closed["state"] == "closed" and closed["cleanup_error"] is None, closed
            metrics = console.request(SOURCE, {"action": "status"})
            for kind in ("video", "audio"):
                assert metrics[kind]["active"] == 0 and metrics[kind]["opened"] == metrics[kind]["closed"] == baseline[kind]["opened"] + 1, metrics
            verify_video(collect(video, ("127.0.0.1", ports["video"])), key, expected_video)
            verify_audio(collect(audio, ("127.0.0.1", ports["audio"])), key, 17)
        finally:
            stop_client(control)
            if association:
                association.close()


def exercise_mismatch(console):
    key = secrets.token_bytes(16)
    baseline = console.request(SOURCE, {"action": "status"})
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        opened = request(console, ticket_config(video, audio, key, "mismatched-profile"))
        assert opened["state"] == "ready", opened
        client = RtspClient(opened["local"], opened["rtsp_url"], key)
        ports = client.setup()
        wrong = announcement().replace(b"clientViewportWd:16", b"clientViewportWd:32")
        client.transact("ANNOUNCE", "streamid=control/13/0", {"Content-Type": "application/sdp"}, wrong, expected=406)
        closed = request(console, {"action": "close", "ticket": opened["ticket"]})
        assert closed["state"] in ("closed", "failed") and closed["cleanup_error"] is None, closed
        metrics = console.request(SOURCE, {"action": "status"})
        for kind in ("video", "audio"):
            assert metrics[kind]["active"] == 0 and metrics[kind]["opened"] == metrics[kind]["closed"] == baseline[kind]["opened"], metrics
        assert collect(video, ("127.0.0.1", ports["video"])) == []
        assert collect(audio, ("127.0.0.1", ports["audio"])) == []


def main():
    setting = os.environ.get("AMATERASU_ENET_CLIENT_ORACLE")
    if not setting:
        raise SystemExit("Set AMATERASU_ENET_CLIENT_ORACLE using scripts/control_enet_oracle/build.sh")
    oracle = Path(setting).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-rtsp-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        settings = bootstrap["policy"]["settings"]
        settings["amaterasu.rtsp"] = {"enabled": True, "bind": "127.0.0.1:0", "allowed_principals": ["amaterasu.console"]}
        settings["amaterasu.audio-encode"] = {"enabled": True}
        settings["amaterasu.stream-session"] = {"enabled": True, "allowed_principals": ["amaterasu.rtsp"]}
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {"enabled": True, "allowed_principals": ["amaterasu.stream-session"]}
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            exercise(console, oracle, directory)
            exercise(console, oracle, directory, discovery=True)
            exercise_mismatch(console)
            console.eof()
        finally:
            console.close()
    print("Passed: signed encrypted RTSP negotiation and cross-connection replay protection; fixed and token-discovered UDP destinations, rejected malformed/wrong-lane pings and audio rebinding, no capture before both destinations and authenticated control, actual Opus/encrypted UDP with synthetic capture, confirmed TEARDOWN cleanup and unsupported ANNOUNCE rollback. Public launch and hardware/client qualification remain open.")


if __name__ == "__main__":
    main()
