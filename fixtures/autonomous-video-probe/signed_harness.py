#!/usr/bin/env python3
"""Isolated development-signed synthetic source -> video -> x264 probe."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tempfile
import time

FIXTURE = Path(__file__).resolve().parent
WORKSPACE = FIXTURE.parents[1]
sys.path.insert(0, str(WORKSPACE / "scripts"))
from test_component_updates import run
from supervisor import SessionProcess

if not __debug__:
    raise RuntimeError("optimized Python disables required harness assertions")

CAPTURE = "test.custom-capture/1"


def configure(h, nonce, deny):
    path = h.root / "bootstrap.json"
    data = json.loads(path.read_text())
    policy = data["policy"]
    settings = policy["settings"]
    settings["amaterasu.pairing"]["host_configuration"] = False
    settings["amaterasu.management"] = {"enabled": False, "bind": "127.0.0.1:0", "allowed_origins": ["http://127.0.0.1:3000"]}
    settings["amaterasu.catalog"]["export_principals"] = {"amaterasu.nvhttp": "amaterasu.console"}
    settings["amaterasu.nvhttp"] = {"enabled": False, "http_bind": "127.0.0.1:0", "https_bind": "127.0.0.1:0", "streaming": {"capture": {"capture_provider": "custom", "output": "nv12-fixture-display", "encoder": 0, "audio_endpoint": "default"}, "profile": {"max_width": 64, "max_height": 48, "max_fps": 30, "bitrate_kbps": 1000, "max_reference_frames": 1, "packet_size": 1024, "fec_percentage": 20, "min_parity_shards": 2, "max_fec_blocks": 1, "packet_ms": 10, "audio_bitrate": 128000, "encrypt_video": True, "encrypt_audio": True, "permit_legacy_audio_iv_wrap": False}}}
    settings["amaterasu.rtsp"] = {"enabled": False, "bind": "127.0.0.1:0", "allowed_principals": ["amaterasu.nvhttp"]}
    settings["amaterasu.stream-session"] = {"enabled": True, "enable_input": False, "allowed_principals": ["amaterasu.rtsp"]}
    settings["amaterasu.capture-source"] = {"custom_service": True}
    settings["amaterasu.software-probe-audio"] = {"enable_video": False}
    settings["amaterasu.video"] = {"enabled": True, "capture_providers": [], "external_capture_providers": {"custom": CAPTURE}}
    video = policy["principals"]["amaterasu.video"]
    video["call"].append(CAPTURE)
    video["native_use"].append(CAPTURE)
    for principal in ("amaterasu.nvhttp", "amaterasu.rtsp", "amaterasu.stream-session", "amaterasu.media-probe"):
        policy["principals"][principal]["call"].append(CAPTURE)
    settings["amaterasu.encode"] = {"enabled": True}
    settings["amaterasu.audio-encode"] = {"enabled": True}
    for component in ("media-transport", "control-transport"):
        settings[f"amaterasu.{component}"] = {"enabled": True, "allowed_principals": ["amaterasu.stream-session"]}
    settings["amaterasu.media-probe"] = {"enabled": True, "acknowledge_capture": "synthetic-test-only", "output": "nv12-fixture-display", "width": 64, "height": 48, "nonce": nonce}
    if deny:
        policy["principals"]["amaterasu.media-probe"]["native_use"].remove("amaterasu.video/1")
    path.write_text(json.dumps(data))


def run_case(target, fixture_library, deny):
    nonce = secrets.token_hex(16)
    result_path = Path(tempfile.gettempdir()) / f"amaterasu-autonomous-video-{nonce}.h264"
    with tempfile.TemporaryDirectory(prefix="amaterasu-autonomous-video-") as temporary:
        root = Path(temporary)
        class Harness: pass
        h = Harness()
        h.root, h.host, h.publisher = root / "host", target / "amaterasu", target / "amaterasu-release"
        h.artifacts = root / "artifacts"
        h.artifacts.mkdir()
        for source in target.glob("libamaterasu_*.so"):
            shutil.copy2(source, h.artifacts / source.name)
        # The existing software-probe profile names this development-only principal
        # and library. Only the isolated artifact copy is replaced for this test.
        shutil.copy2(fixture_library, h.artifacts / "libamaterasu_media_probe.so")
        run(h.publisher, "init", h.root)
        configure(h, nonce, deny)
        package = root / "package"
        run(h.publisher, "pack-software-probe", h.root, h.artifacts, package, "1")
        run(h.publisher, "verify", h.root, package)
        run(h.publisher, "stage", h.root, package)
        console = SessionProcess([str(h.host), "serve", str(h.root)])
        try:
            deadline = time.monotonic() + 40
            while time.monotonic() < deadline:
                logs = "".join(console.errors)
                if "AUTONOMOUS_VIDEO_PROBE_RESULT " in logs or "AUTONOMOUS_VIDEO_PROBE_FAILURE " in logs or console.process.poll() is not None:
                    break
                time.sleep(0.05)
            logs = "".join(console.errors)
            if deny:
                assert not result_path.exists() and "AUTONOMOUS_VIDEO_PROBE_RESULT " not in logs, logs
                matches = [line for line in logs.splitlines() if "AUTONOMOUS_VIDEO_PROBE_FAILURE " in line]
                assert matches, logs
                line = matches[-1]
                failure = json.loads(line.split("AUTONOMOUS_VIDEO_PROBE_FAILURE ", 1)[1])
                assert "native binding failed" in failure["failure"] and "permission_denied" in failure["failure"], failure
                assert failure["cleanup"] == "Some(Clean)", failure
                print("Native video binding denied; cleanup confirmed")
            else:
                matches = [line for line in logs.splitlines() if "AUTONOMOUS_VIDEO_PROBE_RESULT " in line]
                assert matches, logs
                line = matches[-1]
                result = json.loads(line.split("AUTONOMOUS_VIDEO_PROBE_RESULT ", 1)[1])
                assert Path(result["path"]) == result_path and result["cleanup"] == "confirmed", result
                assert result["access_units"] >= 8 and result["keyframe"], result
                subprocess.run([sys.executable, str(FIXTURE / "verify.py"), str(result_path), "64", "48"], check=True, timeout=20)
            console.eof()
        finally:
            try:
                console.close()
            finally:
                result_path.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target")) / "debug", help="directory with the host, release tool and development libraries")
    parser.add_argument("--fixture-library", type=Path, help="fixture library, if built in a different target directory")
    args = parser.parse_args()
    target = args.target.resolve()
    library = (args.fixture_library or target / "libamaterasu_autonomous_video_probe.so").resolve()
    run_case(target, library, False)
    run_case(target, library, True)

if __name__ == "__main__":
    main()
