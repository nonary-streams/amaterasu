#!/usr/bin/env python3
"""Public encrypted Auto or software stream from controlled NV12 through x264.

The signed probe has genuine video/encoder/transport modules and test-only
capture/audio sources. Auto mode qualifies selection of the sole software x264
candidate; the explicit software mode remains available for comparison. FFmpeg
independently decodes the H.264 UDP payload. This does not qualify desktop
capture, a game, performance, packaging or Moonlight playback. Default encoder
artifacts remain feature-disabled.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import socket
import struct
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit

from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from test_component_updates import CARGO, ComponentHarness, run
from test_control_transport import encrypt_control, wait_connected
from test_host_service import Console
from test_management_api import Api, ORIGIN, PASSWORD, available_moonlight_port_pair
from test_native_media import inverse, parity_row
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_public_launch import child_running, create_application, launch_query, wait_until
from test_rtsp import PeerDiscovery, RtspClient, announcement
from test_stream_session import collect, stop_client, verify_audio

CAPTURE = "test.capture-source/1"
CUSTOM_CAPTURE = "test.custom-capture/1"
AUDIO = "test.stream-source/1"
WIDTH, HEIGHT, SOURCE_FRAMES = 64, 48, 12


def wait_ready(console):
    deadline = time.monotonic() + 120
    while not any("Development worker ready: pid=" in line for line in console.errors):
        assert console.process.poll() is None, "".join(console.errors)
        assert time.monotonic() < deadline, "worker did not become ready within 120 seconds"
        time.sleep(0.05)


def checked(api, path, payload=None, bearer=None):
    status, result, _ = api.post(path, payload, bearer)
    assert status == 200 and result.get("ok"), (path, status, result)
    return result


def configure(harness, custom_capture=False, direct=False):
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    settings = bootstrap["policy"]["settings"]
    settings["amaterasu.pairing"]["host_configuration"] = False
    settings["amaterasu.management"] = {
        "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
    }
    settings["amaterasu.catalog"]["export_principals"] = {
        "amaterasu.nvhttp": "amaterasu.console",
    }
    base_port = available_moonlight_port_pair()
    settings["amaterasu.nvhttp"] = {
        "enabled": True,
        "http_bind": f"127.0.0.1:{base_port}",
        "https_bind": f"127.0.0.1:{base_port - 5}",
        "streaming": {
            "capture": {
                "capture_provider": "custom" if custom_capture else "dxgi", "output": "nv12-fixture-display",
                "encoder": 0, "audio_endpoint": "default",
            },
            "profile": {
                "max_width": WIDTH, "max_height": HEIGHT, "max_fps": 30,
                "bitrate_kbps": 1000, "max_reference_frames": 1,
                "packet_size": 1024, "fec_percentage": 20,
                "min_parity_shards": 2, "max_fec_blocks": 1,
                "packet_ms": 10, "audio_bitrate": 128000,
                "encrypt_video": True, "encrypt_audio": True,
                "permit_legacy_audio_iv_wrap": False,
            },
        },
    }
    settings["amaterasu.rtsp"] = {
        "enabled": True, "bind": f"127.0.0.1:{base_port + 21}",
        "allowed_principals": ["amaterasu.nvhttp"],
    }
    settings["amaterasu.stream-session"] = {
        "enabled": True, "enable_input": False,
        "allowed_principals": ["amaterasu.rtsp"],
    }
    settings["amaterasu.capture-source"] = {"custom_service": True} if custom_capture else {}
    settings["amaterasu.software-probe-audio"] = {"enable_video": False}
    settings["amaterasu.video"] = {"enabled": True, "capture_providers": [] if custom_capture else ["dxgi"]}
    if custom_capture:
        settings["amaterasu.video"]["external_capture_providers"] = {"custom": CUSTOM_CAPTURE}
        # The development policy composes several profiles, with the later
        # platform video entry replacing the software-probe entry. Explicitly
        # authorize this test-only provider for this isolated host.
        video_policy = bootstrap["policy"]["principals"]["amaterasu.video"]
        video_policy["call"].append(CUSTOM_CAPTURE)
        video_policy["native_use"].append(CUSTOM_CAPTURE)
        assert CUSTOM_CAPTURE in bootstrap["policy"]["principals"]["amaterasu.console"]["call"]
        for principal in ("amaterasu.nvhttp", "amaterasu.rtsp", "amaterasu.stream-session"):
            bootstrap["policy"]["principals"][principal]["call"].append(CUSTOM_CAPTURE)
        if direct:
            bootstrap["policy"]["principals"]["amaterasu.media-probe"]["call"].append(CUSTOM_CAPTURE)
    settings["amaterasu.encode"] = {"enabled": True}
    settings["amaterasu.audio-encode"] = {"enabled": True}
    if direct:
        settings["amaterasu.nvhttp"]["enabled"] = False
        settings["amaterasu.rtsp"]["enabled"] = False
    for component in ("media-transport", "control-transport"):
        settings[f"amaterasu.{component}"] = {
            "enabled": True, "allowed_principals": ["amaterasu.stream-session"],
        }
    path.write_text(json.dumps(bootstrap))
    package = harness.directory / "software-package"
    run(harness.publisher, "pack-software-probe", harness.root, harness.artifacts, package, "1")
    run(harness.publisher, "verify", harness.root, package)
    manifest = json.loads((package / "manifest.json").read_text())
    libraries = {entry["id"]: entry["file"] for entry in manifest["components"]}
    assert "amaterasu_capture_source" in libraries["amaterasu.capture-source"]
    assert "amaterasu_video" in libraries["amaterasu.video"]
    assert "amaterasu_encode" in libraries["amaterasu.encode"]
    assert "amaterasu.capture-dxgi" not in libraries
    if custom_capture:
        components = {entry["id"]: entry for entry in manifest["components"]}
        provider = components["amaterasu.capture-source"]["requested"]
        video_grants = components["amaterasu.video"]["requested"]
        assert CUSTOM_CAPTURE in provider["provide"] and CUSTOM_CAPTURE in provider["native_provide"], provider
        assert CUSTOM_CAPTURE in video_grants["call"] and CUSTOM_CAPTURE in video_grants["native_use"], video_grants
        for principal in ("amaterasu.nvhttp", "amaterasu.rtsp", "amaterasu.stream-session"):
            assert CUSTOM_CAPTURE in components[principal]["requested"]["call"], components[principal]
    harness.stage(package)


def initialize(console, encoder_selector):
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
        "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
    })
    assert imported["ok"], imported
    state = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
                     lambda result: result.get("tls_ready"))
    http, https = "http://" + state["http_address"], "https://" + state["https_address"]
    assert console.request("amaterasu.authentication/1", {
        "action": "setup", "username": "administrator", "password": PASSWORD,
    })["ok"]
    api = Api(console)
    bearer = checked(api, "/v1/auth/login", {
        "username": "administrator", "password": PASSWORD,
    })["bearer"]
    settings = checked(api, "/v1/nvhttp/settings", bearer=bearer)
    settings = checked(api, "/v1/nvhttp/settings_update", {
        "hostname": settings["hostname"], "port": settings["port"],
        "expected_revision": settings["revision"], "lan_encryption_mode": 2,
        "sw_preset": "superfast", "sw_tune": "zerolatency", "encoder": encoder_selector,
        "min_threads": 3,
    }, bearer)
    assert (settings["effective_sw_preset"], settings["effective_sw_tune"],
            settings["configured_encoder"], settings["configured_min_threads"],
            settings["effective_min_threads"]) == ("superfast", "zerolatency", encoder_selector, 3, 3), settings
    encoders = checked(api, "/v1/media/encoders", bearer=bearer)
    assert encoders["software_encoder"]["available"], encoders
    candidates = encoders["encoders"]
    assert len(candidates) == 1, candidates
    assert candidates[0]["identity"]["clsid"] == "amaterasu.software.x264", candidates
    assert candidates[0]["hardware"] is False, candidates
    pair(console, http)
    return https, tls_context()


def start_control(oracle, directory, port, key, connect_data):
    payload = directory / "software-idr.bin"
    # Start authenticates the first control message without requesting an IDR.
    # The later sequence-one message is the only explicit ForceIDR in this test.
    payload.write_bytes(encrypt_control(key, 0x0307, bytes(1)))
    control = subprocess.Popen(
        [str(oracle), str(port), str(connect_data), str(payload), "--interactive"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    try:
        wait_connected(control)
    except BaseException:
        stop_client(control)
        raise
    return control


def request_later_idr(control, key):
    plaintext = b"\x02\x03\x02\x00\x00\x00"
    sequence = (1).to_bytes(4, "little")
    sealed = AESGCM(key).encrypt(sequence + bytes(6) + b"CC", plaintext, None)
    envelope = b"\x01\x00" + (20 + len(plaintext)).to_bytes(2, "little")
    envelope += sequence + sealed[-16:] + sealed[:-16]
    control.stdin.write(struct.pack("!I", len(envelope)) + envelope)
    control.stdin.flush()


def reassemble_video(datagrams, key):
    assert datagrams, "no encrypted video packets"
    frames = {}
    for sequence, datagram in enumerate(datagrams):
        assert datagram[:12] == sequence.to_bytes(8, "little") + b"\0\0\0V"
        packet = AESGCM(key).decrypt(datagram[:12], datagram[32:] + datagram[16:32], None)
        assert packet[:2] == b"\x90\0" and len(packet) == 1040, (sequence, len(packet))
        assert int.from_bytes(packet[2:4], "big") == sequence
        frame = int.from_bytes(packet[20:24], "little")
        assert int.from_bytes(datagram[12:16], "little") == frame
        assert packet[27] == 0, "unexpected multi-block video"
        fec = int.from_bytes(packet[28:32], "little")
        index, count = (fec >> 12) & 1023, fec >> 22
        assert 0 < count <= 16
        parts = frames.setdefault(frame, {})
        assert index not in parts, "duplicate encrypted shard"
        parts[index] = packet
    assert sorted(frames) == list(range(1, len(frames) + 1)), sorted(frames)
    units, timestamps, idrs = [], [], []
    for frame, parts in sorted(frames.items()):
        count = int.from_bytes(parts[0][28:32], "little") >> 22
        parity_count = max(2, (count * 20 + 99) // 100)
        assert sorted(parts) == list(range(count + parity_count)), (frame, parts.keys())
        timestamp = int.from_bytes(parts[0][4:8], "big")
        assert all(int.from_bytes(packet[4:8], "big") == timestamp for packet in parts.values())
        timestamps.append(timestamp)
        shards = []
        for index in range(count):
            shard = bytearray(parts[index])
            shard[:16] = bytes(16)
            shard[28:32] = bytes(4)
            shards.append(shard)
        for index in range(parity_count):
            parity = parity_row([inverse((parity_count + column) ^ index)
                                 for column in range(count)], shards)
            packet = parts[count + index]
            assert packet[16:20] == parity[16:20]
            assert packet[24:27] == parity[24:27]
            assert packet[32:] == parity[32:]
        payload = b"".join(parts[index][32:] for index in range(count))
        # Bytes 1 and 2 carry the measured host processing latency.
        assert payload[0] == 1 and payload[6:8] == bytes(2), payload[:8].hex()
        final_size = int.from_bytes(payload[4:6], "little")
        assert 0 < final_size <= 1008
        actual_size = (count - 1) * 1008 + final_size
        assert not any(payload[actual_size:]), "nonzero FEC padding"
        unit = payload[8:actual_size]
        kinds = [part[0] & 31 for part in re.split(b"\x00\x00\x00?\x01", unit)[1:] if part]
        assert unit.startswith(b"\x00\x00\x00\x01") and any(kind in (1, 5) for kind in kinds)
        idr = 5 in kinds
        assert payload[3] == (2 if idr else 1), (frame, kinds)
        idrs.append(idr)
        units.append(unit)
    assert len(units) >= SOURCE_FRAMES, len(units)
    # Real video uses elapsed time since Open, including provider allocation;
    # its first timestamp therefore need not be the fixture's initial zero.
    assert all(left < right for left, right in zip(timestamps, timestamps[1:])), timestamps
    assert idrs[0] and any(idrs[3:]), "post-activation ENet IDR did not reach x264"
    assert any(not idr for idr in idrs[1:3]), "no P picture preceded the requested IDR"
    return units, timestamps, idrs


def independently_decode(units, directory):
    elementary = directory / "software-stream.h264"
    elementary.write_bytes(b"".join(units))
    result = subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-threads", "1",
        "-f", "h264", "-i", str(elementary), "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1",
    ], capture_output=True, timeout=15)
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    frame_bytes = WIDTH * HEIGHT
    assert len(result.stdout) == len(units) * frame_bytes, (len(result.stdout), len(units))
    contrasts = []
    for index in range(SOURCE_FRAMES):
        pixels = result.stdout[index * frame_bytes:(index + 1) * frame_bytes]
        left = sum(pixels[row * WIDTH + column] for row in range(HEIGHT)
                   for column in range(4, WIDTH // 2 - 4)) / (HEIGHT * (WIDTH // 2 - 8))
        right = sum(pixels[row * WIDTH + column] for row in range(HEIGHT)
                    for column in range(WIDTH // 2 + 4, WIDTH - 4)) / (HEIGHT * (WIDTH // 2 - 8))
        contrasts.append(left - right)
        assert abs(left - right) > 100, (index, left, right)
    assert all(left * right < 0 for left, right in zip(contrasts, contrasts[1:])), contrasts


def exercise(console, https, context, oracle, directory, custom_capture=False):
    marker = directory / "software-application.pid"
    app_id = create_application(console, marker, 0, 60)
    key = secrets.token_bytes(16)
    query = {**launch_query(key, app_id), "mode": f"{WIDTH}x{HEIGHT}x30"}
    opened = successful(get(https, "/launch", query, context))
    url = opened.findtext("sessionUrl0")
    assert url and url.startswith("rtspenc://"), url
    wait_until(lambda: marker.exists(), "public launch did not start its controlled process")
    pid = int(marker.read_text())
    assert child_running(pid)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        association = PeerDiscovery(video, audio)
        target = urlsplit(url)
        client = RtspClient(f"{target.hostname}:{target.port}", url, key)
        ports = client.setup(association.on_setup)
        sdp = announcement().replace(b"clientViewportWd:16", b"clientViewportWd:64")
        sdp = sdp.replace(b"clientViewportHt:16", b"clientViewportHt:48")
        client.transact("ANNOUNCE", "streamid=control/13/0", {"Content-Type": "application/sdp"}, sdp)
        client.transact("PLAY", "/")
        control = start_control(oracle, directory, ports["control"], key, ports["connect_data"])
        try:
            # The real video path is still closed until the selected UDP peer proves its token.
            assert console.request(CAPTURE, {"action": "status"})["opened"] == 0
            association.send("video")
            wait_until(lambda: console.request(CAPTURE, {"action": "status"})["frames"] >= 3,
                       "real video did not accept controlled NV12 frames")
            request_later_idr(control, key)
            wait_until(lambda: console.request(CAPTURE, {"action": "status"})["frames"] == SOURCE_FRAMES,
                       "real video did not encode the finite NV12 source")
            client.transact("TEARDOWN", "/")
            wait_until(lambda: console.request(CAPTURE, {"action": "status"})["active"] == 0,
                       "public teardown retained a capture lease")
            wait_until(lambda: not child_running(pid), "public teardown retained its application")
            capture = console.request(CAPTURE, {"action": "status"})
            assert capture["opened"] == capture["closed"] == 1 and capture["frames"] == SOURCE_FRAMES, capture
            assert capture["callers"]["amaterasu.video"] == {
                "opened": 1, "closed": 1, "frames": SOURCE_FRAMES, "active": 0,
            }, capture
            source = console.request(AUDIO, {"action": "status"})
            assert source["video"]["opened"] == source["video"]["frames"] == 0, source
            assert source["audio"]["opened"] == source["audio"]["closed"] == 1, source
            assert source["audio"]["active"] == 0 and source["audio"]["frames"] == 8, source
            units, timestamps, idrs = reassemble_video(collect(video, ("127.0.0.1", ports["video"])), key)
            independently_decode(units, directory)
            verify_audio(collect(audio, ("127.0.0.1", ports["audio"])), key, 17)
            return {"decoded_frames": len(units), "source_frames": SOURCE_FRAMES,
                    "idr_frames": sum(idrs), "rtp_timestamps": timestamps,
                    "capture_leases_opened": capture["opened"], "capture_leases_closed": capture["closed"]}
        finally:
            stop_client(control)
            association.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--encoder", choices=("auto", "software"), default="auto")
    parser.add_argument("--custom-capture", action="store_true", help="route through the independent test capture service")
    arguments = parser.parse_args()
    oracle = Path(os.environ["AMATERASU_ENET_CLIENT_ORACLE"]).resolve(strict=True)
    # Use a dedicated target directory: enabling x264 must not overwrite default artifacts.
    target = Path(os.environ["CARGO_TARGET_DIR"])
    assert target.is_absolute(), "software qualification requires an explicit isolated Cargo target"
    run(CARGO, "build", "--workspace", "--locked", "--offline",
        "--features", "amaterasu-encode/software-x264")
    with tempfile.TemporaryDirectory(prefix="amaterasu-public-software-stream-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        configure(harness, arguments.custom_capture)
        console = Console(harness)
        try:
            wait_ready(console)
            https, context = initialize(console, arguments.encoder)
            evidence = exercise(console, https, context, oracle, directory, arguments.custom_capture)
            console.eof()
        except BaseException:
            print("".join(console.errors), file=sys.stderr)
            raise
        finally:
            console.close()
    print(f"Passed: public {arguments.encoder} selection, signed settings with min_threads=3, paired HTTPS launch, encrypted RTSP/ENet/video, real NV12 video/x264, independent FFmpeg decode, post-activation IDR, FEC, graceful drain and owned-source cleanup.")
    print(json.dumps(evidence, sort_keys=True))
    print("Not qualified: desktop capture, game/client playback, performance, Windows runtime or production x264 distribution; configuration parity IDs remain pending.")


if __name__ == "__main__":
    main()
