#!/usr/bin/env python3
"""Sustain real Moonlight protocol receive + H.264/Opus decoding on loopback.

The caller builds scripts/moonlight_protocol_oracle/client.c against a separately
supplied Moonlight library. This uses isolated signed fixture hosts, never the
running service, real display/audio devices, input, or existing pairing state.
Public NVHTTP pairing/launch and native capture are separate acceptance gates.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import secrets
import subprocess
import tempfile

from test_component_updates import ComponentHarness, run
from test_host_service import Console
from test_rtsp import request
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_public_launch import child_running, create_application, launch_query, wait_until


def exercise(oracle: Path, milliseconds: int, public_launch: bool) -> None:
    with tempfile.TemporaryDirectory(prefix="amaterasu-sustained-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        settings = bootstrap["policy"]["settings"]
        settings["amaterasu.rtsp"] = {
            "enabled": True, "bind": "127.0.0.1:0",
            "allowed_principals": ["amaterasu.console"],
        }
        settings["amaterasu.stream-source"] = {"sustain_ms": milliseconds + 20000}
        if public_launch:
            settings["amaterasu.pairing"]["host_configuration"] = False
            settings["amaterasu.catalog"]["export_principals"] = {"amaterasu.nvhttp": "amaterasu.console"}
            settings["amaterasu.nvhttp"] = {
                "enabled": True, "http_bind": "127.0.0.1:0", "https_bind": "127.0.0.1:0",
                "streaming": {
                    "capture": {"capture_provider": "dxgi", "output": "synthetic-display",
                                "encoder": 0, "audio_endpoint": "default"},
                    "profile": {"max_width": 16, "max_height": 16, "max_fps": 30,
                                "bitrate_kbps": 16000, "packet_size": 992, "packet_ms": 5,
                                "max_reference_frames": 1, "fec_percentage": 20,
                                "min_parity_shards": 2, "max_fec_blocks": 1,
                                "audio_bitrate": 128000, "encrypt_video": True,
                                "encrypt_audio": True, "permit_legacy_audio_iv_wrap": False},
                },
            }
            settings["amaterasu.rtsp"]["allowed_principals"] = ["amaterasu.nvhttp"]
        settings["amaterasu.audio-encode"] = {"enabled": True}
        settings["amaterasu.stream-session"] = {
            "enabled": True, "allowed_principals": ["amaterasu.rtsp"],
        }
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {
                "enabled": True, "allowed_principals": ["amaterasu.stream-session"],
            }
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            if public_launch:
                imported = console.request("amaterasu.pairing/1", {
                    "action": "import_identity",
                    "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
                    "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
                })
                assert imported["ok"], imported
                state = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda s: s.get("tls_ready"))
                http, https = "http://" + state["http_address"], "https://" + state["https_address"]
                pair(console, http)
                context = tls_context()
                marker = directory / "application.pid"
                app_id = create_application(console, marker, 0, milliseconds / 1000 * 2 + 90)
            for attempt in range(2):
                key = secrets.token_bytes(16)
                key_path = directory / "launch-key.bin"
                key_path.touch(mode=0o600, exist_ok=True)
                key_path.write_bytes(key)
                configuration = {
                    "action": "open", "operation_id": f"sustained-{attempt}",
                    "peer_ip": "127.0.0.1", "discover_peers": True,
                    "video_port": 0, "audio_port": 0,
                    "key_hex": key.hex(), "key_id": 17, "connect_data": secrets.randbits(32),
                    "capture": {"capture_provider": "dxgi", "output": "synthetic-display",
                                "encoder": 0, "audio_endpoint": "default"},
                    # Moonlight reserves 20% bitrate for FEC, subtracts the
                    # 32-byte video envelope, and selects high-quality 5 ms Opus.
                    "profile": {"width": 16, "height": 16, "fps": 30,
                                "bitrate_kbps": 16000, "packet_size": 992, "packet_ms": 5},
                }
                if public_launch:
                    # Single-controller Moonlight mode includes these hints even
                    # without a controller; they must not enable input injection.
                    query = dict(launch_query(key, app_id), sops="1", psmap="1",
                                 gcmap="1", remoteControllersBitmap="1", gcpersist="1")
                    opened_xml = successful(get(https, "/launch", query, context))
                    opened = {"ok": True, "state": "ready", "rtsp_url": opened_xml.findtext("sessionUrl0")}
                    wait_until(marker.exists, "public launch did not start its owned child")
                    pid = int(marker.read_text())
                else:
                    opened = request(console, configuration)
                assert opened.get("ok") and opened["state"] == "ready", opened
                try:
                    result = subprocess.run(
                        [str(oracle), opened["rtsp_url"], str(key_path), str(milliseconds)],
                        capture_output=True, text=True, timeout=milliseconds / 1000 + 35,
                    )
                    print(result.stdout, flush=True)
                    if result.returncode:
                        print(result.stderr, flush=True)
                        print("".join(console.errors)[-16000:], flush=True)
                        raise AssertionError(f"Moonlight protocol oracle exited {result.returncode}")
                finally:
                    if public_launch:
                        successful(get(https, "/cancel", context=context))
                        wait_until(lambda: not child_running(pid), "public cancel did not reap its owned child")
                        marker.unlink(missing_ok=True)
                    else:
                        closed = request(console, {"action": "close", "ticket": opened["ticket"]})
                        assert closed["state"] in ("closed", "failed") and closed["cleanup_error"] is None, closed
                    metrics = console.request("test.stream-source/1", {"action": "status"})
                    for lane in ("video", "audio"):
                        assert metrics[lane]["active"] == 0, metrics
                        assert metrics[lane]["opened"] == metrics[lane]["closed"], metrics
                    inputs = console.request("test.input-probe/1", {"action": "status"})
                    assert not inputs["events"], "controller hints authorized input effects"
                    assert inputs["acquired"] == 0 and not inputs["active"], inputs
            console.eof()
        except BaseException:
            print("".join(console.errors)[-16000:], flush=True)
            raise
        finally:
            console.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("oracle", type=Path)
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--public-launch", action="store_true", help="pair over HTTP and launch/cancel through pinned HTTPS")
    arguments = parser.parse_args()
    if not 1 <= arguments.seconds <= 180:
        parser.error("--seconds must be 1 through 180")
    exercise(arguments.oracle.resolve(strict=True), arguments.seconds * 1000, arguments.public_launch)
    print("Passed two sustained Moonlight connections, continuous decoded H.264/non-silent Opus, encrypted control, peer discovery and owned cleanup.")


if __name__ == "__main__":
    main()
