#!/usr/bin/env python3
"""Start the opt-in signed profile in an isolated, effects-disabled host root.

Use a coherent artifact directory and independent provider build. This opens
only loopback listeners and never installs a service or touches the desktop.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

WORKSPACE = Path(__file__).resolve().parents[1]


def run(*arguments: object, environment: dict[str, str]) -> str:
    result = subprocess.run([str(argument) for argument in arguments], cwd=WORKSPACE,
                            env=environment, text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise AssertionError(f"command failed: {arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def available_base_port() -> int:
    # HTTP, HTTPS and RTSP occupy offsets 0, -5 and +21. Probe only loopback.
    for base in range(51000, 60000, 37):
        sockets = []
        try:
            for port in (base, base - 5, base + 21):
                listener = socket.socket()
                listener.bind(("127.0.0.1", port))
                sockets.append(listener)
            return base
        except OSError:
            pass
        finally:
            for listener in sockets:
                listener.close()
    raise RuntimeError("no available loopback ports")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--enable-display-actions", action="store_true",
                        help="also verify startup with explicit remove-owned action policy")
    parser.add_argument("publisher", type=Path)
    parser.add_argument("host", type=Path)
    parser.add_argument("artifacts", type=Path)
    parser.add_argument("provider_repo", type=Path)
    parser.add_argument("provider_library", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="amaterasu-session-profile-") as temporary:
        directory = Path(temporary)
        home = directory / "home"
        runtime = home / "run"
        runtime.mkdir(parents=True, mode=0o700)
        environment = os.environ.copy()
        for name in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_ID",
                     "XDG_SESSION_TYPE", "PULSE_SERVER", "PULSE_RUNTIME_PATH", "PIPEWIRE_RUNTIME_DIR",
                     "XAUTHORITY", "WAYLAND_SOCKET", "KDE_FULL_SESSION"):
            environment.pop(name, None)
        environment.update(HOME=str(home), XDG_RUNTIME_DIR=str(runtime))
        root, package = directory / "root", directory / "package"
        prepare = ["python3", WORKSPACE / "scripts/prepare_session_extension_profile.py"]
        if args.enable_display_actions:
            prepare.append("--enable-display-actions")
        prepare.extend([args.publisher, root, args.artifacts, package, 1, args.provider_repo,
                        args.provider_library, "--", "--lan", f"127.0.0.1:{available_base_port()}"])
        run(*prepare, environment=environment)
        policy = json.loads((root / "bootstrap.json").read_text())["policy"]["settings"]
        action_ids = policy["amaterasu.session-lifecycle"]["input_actions"]
        action_grants = policy["amaterasu.extension-actions"]["grants"]
        if args.enable_display_actions:
            assert action_ids == ["nonary.display-settings/remove-owned"] and len(action_grants) == 3
        else:
            assert action_ids == [] and action_grants == []
        run(args.publisher, "stage", root, package, environment=environment)

        def request(payload: dict) -> dict:
            envelope = {"op": "request", "service": "amaterasu.extension-registry/1", "payload": payload,
                        "timeout_ms": 8000, "operation_id": "session-extension-profile-test"}
            response = json.loads(run(args.host, "probe", root, json.dumps(envelope), environment=environment))
            assert response["kind"] == "result", response
            return response["payload"]

        listing = request({"operation": "list", "after": None, "limit": 32, "revision": None})
        assert listing["ok"] and [item["provider"] for item in listing["items"]] == ["nonary.display-settings"], listing
        scope = {"kind": "host"}
        current = request({"operation": "settings_get", "provider": "nonary.display-settings", "scope": scope,
                           "application": None, "paired_client": None, "session": None})
        assert current["ok"] and current["revision"] == 0 and current["effective"]["enabled"]["value"] is False, current
        changed = request({"operation": "settings_update", "provider": "nonary.display-settings", "scope": scope,
                           "expected_revision": 0, "values": {"enabled": False, "width": 1921}})
        assert changed["ok"] and changed["revision"] == 1, changed
        reloaded = request({"operation": "settings_get", "provider": "nonary.display-settings", "scope": scope,
                            "application": None, "paired_client": None, "session": None})
        assert reloaded["ok"] and reloaded["revision"] == 1 and reloaded["values"]["width"] == 1921, reloaded
    print("Passed isolated signed profile startup, registry discovery and host settings readback.")
    print("No action, display effect, capture, live streaming or hardware behavior was exercised.")


if __name__ == "__main__":
    main()
