#!/usr/bin/env python3
"""Qualify an independently built library provider in an isolated signed development set.

Requires prebuilt Linux-session artifacts, release/host executables, and the two
standalone SDK provider cdylib in CARGO_TARGET_DIR/debug. The test creates and
removes private roots under TMPDIR, enables no device or public listener, and
does not install a system service. It tests provider discovery, not real game
lifecycle, hardware capture, or client playback.
"""
from __future__ import annotations

import json
import os
import queue
from pathlib import Path
import subprocess
import tempfile

from test_linux_session_profile import isolated_environment
from test_host_service import Console
from types import SimpleNamespace

WORKSPACE = Path(__file__).resolve().parents[1]
LIBRARY = "amaterasu.library/1"
PROVIDER = "example.library.provider/1"


class PatientConsole(Console):
    def receive(self):
        try:
            reply = self.replies.get(timeout=120)
        except queue.Empty as exc:
            raise AssertionError("timed out waiting for host reply: " + "".join(self.errors)) from exc
        assert reply is not None, "".join(self.errors)
        return reply


def run(*args: object, env: dict[str, str] | None = None, fail: bool = False) -> str:
    result = subprocess.run([str(arg) for arg in args], cwd=WORKSPACE, env=env,
                            text=True, capture_output=True, timeout=120)
    if (result.returncode == 0) == fail:
        raise AssertionError(f"unexpected exit: {args}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def manifest(component: str, artifact: Path, service: str) -> dict:
    return {"id": component, "version": "0.1.0", "file": artifact.name,
            "sha256": "0" * 64, "size": 0, "abi_major": 1, "runtime": False,
            "requires": {"amaterasu.runtime": "0.1.0"},
            "requested": {"provide": [service]}}


def request(host: Path, root: Path, environment: dict[str, str], payload: dict,
            service: str = LIBRARY) -> dict:
    command = {"op": "request", "service": service, "payload": payload,
               "timeout_ms": 8000, "operation_id": "signed-library-fixture"}
    result = json.loads(run(host, "probe", root, json.dumps(command), env=environment))
    assert result["kind"] == "result", result
    return result["payload"]


def qualify(directory: Path, artifacts: Path) -> None:
    publisher, host = artifacts / "amaterasu-release", artifacts / "amaterasu"
    root, package = directory / "root", directory / "package"
    environment = isolated_environment(directory)
    run(publisher, "init-linux-session", root)
    bootstrap_path = root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    policy = bootstrap["policy"]
    # Broker-only qualification: do not open listeners or touch the desktop.
    policy["settings"]["amaterasu.rtsp"]["enabled"] = False
    policy["settings"]["amaterasu.nvhttp"]["enabled"] = False
    component = "example.library-provider"
    artifact = artifacts / "libexample_application_library_provider.so"
    template = directory / "component.json"
    template.write_text(json.dumps(manifest(component, artifact, PROVIDER)))
    calls = directory / "calls.json"
    policy["principals"]["amaterasu.console"].setdefault("call", []).extend([LIBRARY, PROVIDER])
    policy["principals"]["amaterasu.library"].setdefault("call", []).append(PROVIDER)
    policy["settings"].setdefault("amaterasu.library", {})["discovery"] = {
        "providers": {"fixture": {"service": PROVIDER,
                                    "allowed_initiators": ["amaterasu.console"]}}}
    policy["settings"][component] = {"callers": {"amaterasu.library": {
        "initiators": ["amaterasu.console"], "discovery": True}}}
    calls.write_text(json.dumps({"amaterasu.library": [PROVIDER]}))
    bootstrap_path.write_text(json.dumps(bootstrap))
    args = [publisher, "pack-linux-session", root, artifacts, package, "1",
            "--external-component", template, artifact, "--call-grants", calls]
    # The native component's declared register grant must be approved separately.
    run(*args, fail=True)
    bootstrap["policy"]["principals"][component] = {"provide": [PROVIDER]}
    bootstrap_path.write_text(json.dumps(bootstrap))
    run(*args)
    run(publisher, "verify", root, package)
    selected = json.loads((package / "manifest.json").read_text())["components"]
    ids = {entry["id"] for entry in selected}
    assert component in ids and "amaterasu.library" in ids, ids
    run(publisher, "stage", root, package)
    console = PatientConsole(SimpleNamespace(host=host, root=root), environment=environment)
    try:
        reply = console.request(LIBRARY, {"action": "discover", "provider": "fixture",
                                          "cursor": None, "limit": 20})
        assert reply["ok"] and reply["entries"][0]["id"] == "fixture-game", reply
        denied = console.request(PROVIDER, {"action": "discover", "cursor": None, "limit": 20})
        assert not denied["ok"] and denied["error"]["code"] == "unauthorized", denied
        forged = console.request(LIBRARY, {"action": "discover", "provider": "fixture",
                                            "cursor": None, "limit": 20, "initiator": "amaterasu.console"})
        assert not forged["ok"], forged
        console.eof()
    finally:
        console.close()



def main() -> None:
    if os.name != "posix":
        raise SystemExit("This qualification uses the Linux session profile")
    artifacts = Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target")).resolve() / "debug"
    with tempfile.TemporaryDirectory(prefix="amaterasu-signed-library-") as temporary:
        qualify(Path(temporary), artifacts)
    print("Passed: signed isolated application-library provider discovery.")
    print("System installation, real application effects, capture and client playback remain unqualified.")


if __name__ == "__main__":
    main()
