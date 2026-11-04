#!/usr/bin/env python3
"""Exercise signed RTX HDR startup and settings in an isolated Linux host.

Requires coherent debug artifacts, a VKMS frame helper, and a verified NVIDIA
runtime installed in a private service-owned parent directory. Opens loopback
listeners only and does not start capture,
display effects, a stream, or install a service. The capture helper is configured
but never invoked. This is not a GPU or Moonlight playback qualification.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import tempfile

from test_linux_session_profile import isolated_environment
from test_session_extension_profile import available_base_port

WORKSPACE = Path(__file__).resolve().parents[1]
PROVIDER = "amaterasu.rtx-hdr"
REGISTRY = "amaterasu.extension-registry/1"
TUNING = {
    "rtx_hdr_enable": True,
    "rtx_hdr_peak_brightness": 1200,
    "rtx_hdr_middle_gray": 65,
    "rtx_hdr_contrast": -25,
    "rtx_hdr_saturation": 35,
    "rtx_hdr_sdr_brightness": 25,
    "rtx_hdr_foreground_only": True,
    "rtx_hdr_fullscreen_only": True,
    "rtx_hdr_debanding": False,
}
SLIDERS = {
    "rtx_hdr_peak_brightness", "rtx_hdr_middle_gray", "rtx_hdr_contrast",
    "rtx_hdr_saturation", "rtx_hdr_sdr_brightness",
}


def run(*arguments: object, environment: dict[str, str]) -> str:
    result = subprocess.run([str(argument) for argument in arguments], cwd=WORKSPACE,
                            env=environment, text=True, capture_output=True, timeout=600)
    if result.returncode:
        raise AssertionError(f"command failed: {arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def envelope(payload: dict, index: int = 0) -> dict:
    return {"op": "request", "service": REGISTRY, "payload": payload,
            "timeout_ms": 8000, "operation_id": f"rtx-hdr-signed-profile.{index}"}


def settings_get() -> dict:
    return {"operation": "settings_get", "provider": PROVIDER, "scope": {"kind": "host"},
            "application": None, "paired_client": None, "session": None}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, default=WORKSPACE / "target/debug")
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--frame-helper", required=True, type=Path)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve(strict=True)
    runtime = args.runtime.resolve(strict=True)
    helper = args.frame_helper.resolve(strict=True)
    if not runtime.is_dir() or not helper.is_file():
        raise ValueError("runtime must be a directory and frame helper must be a file")
    publisher, host = artifacts / "amaterasu-release", artifacts / "amaterasu"
    # Keep the multi-gigabyte signed debug package on the worktree's volume.
    temporary_parent = artifacts.parent
    with tempfile.TemporaryDirectory(prefix="signed-rtx-hdr-", dir=temporary_parent) as temporary:
        directory = Path(temporary)
        environment = isolated_environment(directory)
        root, package = directory / "root", directory / "package"
        run(publisher, "init-linux-session", root, "--lan", f"127.0.0.1:{available_base_port()}",
            "--rtx-hdr-runtime", runtime, "--frame-helper", helper, environment=environment)
        run(publisher, "pack-linux-session-rtx-hdr", root, artifacts, package, "1", environment=environment)
        run(publisher, "verify", root, package, environment=environment)
        run(publisher, "stage", root, package, environment=environment)
        commands = [
            {"operation": "list", "after": None, "limit": 32, "revision": None},
            settings_get(),
            {"operation": "settings_update", "provider": PROVIDER, "scope": {"kind": "host"},
             "expected_revision": 0, "values": TUNING},
            {"operation": "settings_update", "provider": PROVIDER, "scope": {"kind": "host"},
             "expected_revision": 0, "values": {"rtx_hdr_enable": False}},
            # Include a valid change with an invalid one to verify atomic rejection.
            {"operation": "settings_update", "provider": PROVIDER, "scope": {"kind": "host"},
             "expected_revision": 1, "values": {"rtx_hdr_peak_brightness": 2001, "rtx_hdr_contrast": 0}},
            settings_get(),
        ]
        responses = json.loads(run(host, "probe", root, json.dumps([envelope(command, index) for index, command in enumerate(commands)]),
                                   environment=environment))
        assert all(response["kind"] == "result" for response in responses), responses
        listing, current, changed, stale, invalid, reloaded = [response["payload"] for response in responses]
        assert listing["ok"] and PROVIDER in [item["provider"] for item in listing["items"]], listing
        descriptor = next(item for item in listing["items"] if item["provider"] == PROVIDER)
        options = {option["id"]: option for option in descriptor["options"]}
        assert options["rtx_hdr_enable"]["apply_policy"] == "next_session", options
        for option in TUNING.keys() - {"rtx_hdr_enable"}:
            assert options[option]["apply_policy"] == "live_action", (option, options)
        for option in SLIDERS:
            assert options[option]["presentation"] == "slider", (option, options)
        assert current["ok"] and current["revision"] == 0 and not current["effective"]["rtx_hdr_enable"]["value"], current
        assert changed["ok"] and changed["revision"] == 1, changed
        assert not stale["ok"] and stale["error"]["code"] == "revision_conflict", stale
        assert not invalid["ok"], invalid
        assert reloaded["ok"] and reloaded["revision"] == 1, reloaded
        for option, value in TUNING.items():
            assert reloaded["effective"][option]["value"] == value, (option, reloaded)
        # A new worker must read the persisted values rather than process-local state.
        response = json.loads(run(host, "probe", root, json.dumps(envelope(settings_get())), environment=environment))
        assert response["kind"] == "result", response
        persisted = response["payload"]
        assert persisted["ok"] and persisted["revision"] == 1, persisted
        for option, value in TUNING.items():
            assert persisted["effective"][option]["value"] == value, (option, persisted)
    print("Passed isolated signed RTX HDR startup, registry discovery, validation, CAS and persisted settings.")
    print("No capture, display effect, GPU conversion, stream or client playback was exercised.")


if __name__ == "__main__":
    main()
