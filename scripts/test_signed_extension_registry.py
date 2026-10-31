#!/usr/bin/env python3
"""Exercise a signed registry fixture and an independently built settings provider.

Requires the independent provider's examples and built library. Uses a temporary
host root only; no service installation, display mutation, or network listener.
The provider currently exposes settings validation only, not display lifecycle.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

WORKSPACE = Path(__file__).resolve().parents[1]


def run(*arguments: object) -> str:
    result = subprocess.run([str(argument) for argument in arguments], cwd=WORKSPACE,
                            text=True, capture_output=True, timeout=120)
    if result.returncode:
        raise AssertionError(f"command failed: {arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("provider_repo", type=Path)
    parser.add_argument("provider_library", type=Path)
    args = parser.parse_args()
    target = Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target")).resolve() / "debug"
    suffix = ".exe" if os.name == "nt" else ""
    publisher, host = target / f"amaterasu-release{suffix}", target / f"amaterasu{suffix}"
    examples = args.provider_repo / "examples"
    descriptor = json.loads((examples / "settings-descriptor.json").read_text())
    approval = json.loads((examples / "settings-validation-approval.json").read_text())
    provider, service = descriptor["provider"], approval["method"]["service"]
    approval["callers"] = ["amaterasu.extension-registry", "amaterasu.console"]
    descriptor_bytes = json.dumps(descriptor, ensure_ascii=False, separators=(",", ":")).encode()
    descriptor_digest = hashlib.sha256(descriptor_bytes).hexdigest()
    with tempfile.TemporaryDirectory(prefix="amaterasu-signed-registry-") as temporary:
        root = Path(temporary) / "root"
        package = Path(temporary) / "package"
        run(publisher, "init", root)
        bootstrap_path = root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        policy = bootstrap["policy"]
        policy["principals"][provider] = {"provide": [service]}
        policy["principals"]["amaterasu.extension-registry"] = {
            "provide": ["amaterasu.extension-registry/1"], "call": ["amaterasu.state/1", service],
        }
        console = policy["principals"]["amaterasu.console"]
        console["call"] = sorted(set(console.get("call", [])) | {
            "amaterasu.extension-registry/1", "amaterasu.state/1", service,
        })
        policy["api_methods"] = [approval]
        policy["settings"]["amaterasu.extension-registry"] = {"descriptors": [{
            "descriptor": descriptor, "descriptor_sha256": descriptor_digest,
            "approved_methods": [approval["method"]],
            "grants": [{"caller": "amaterasu.console", "scope": {"kind": "host"},
                        "read": True, "write": True}], "migrations": [],
        }]}
        bootstrap_path.write_text(json.dumps(bootstrap))
        calls = Path(temporary) / "calls.json"
        calls.write_text(json.dumps({"amaterasu.extension-registry": [service]}))
        template = examples / ("component-windows.json" if os.name == "nt" else "component-linux.json")
        run(publisher, "pack-registry-fixture", root, target, package, "1", "--external-component",
            template, args.provider_library, "--call-grants", calls)
        run(publisher, "verify", root, package)
        run(publisher, "stage", root, package)

        def request(payload: dict) -> dict:
            command = {"op": "request", "service": "amaterasu.extension-registry/1",
                       "payload": payload, "timeout_ms": 8000, "operation_id": "fixture.registry"}
            response = json.loads(run(host, "probe", root, json.dumps(command)))
            assert response["kind"] == "result", response
            return response["payload"]

        listing = request({"operation": "list", "after": None, "limit": 32, "revision": None})
        assert listing["ok"] and [item["provider"] for item in listing["items"]] == [provider], listing
        described = request({"operation": "describe", "provider": provider})
        assert described["ok"], described
        scope = {"kind": "host"}
        current = request({"operation": "settings_get", "provider": provider, "scope": scope,
                           "application": None, "paired_client": None, "session": None})
        assert current["ok"] and current["revision"] == 0, current
        changed = request({"operation": "settings_update", "provider": provider, "scope": scope,
                           "expected_revision": 0, "values": {"enabled": True}})
        assert changed["ok"] and changed["revision"] == 1, changed
        stale = request({"operation": "settings_update", "provider": provider, "scope": scope,
                         "expected_revision": 0, "values": {"enabled": False}})
        assert not stale["ok"] and stale["error"]["code"] == "revision_conflict", stale
        invalid = request({"operation": "settings_update", "provider": provider, "scope": scope,
                           "expected_revision": 1, "values": {"scale_percent": 123}})
        assert not invalid["ok"] and invalid["error"]["code"] == "invalid_values" and invalid["error"]["message"] == "provider rejected settings", invalid
        reloaded = request({"operation": "settings_get", "provider": provider, "scope": scope,
                            "application": None, "paired_client": None, "session": None})
        assert reloaded["ok"] and reloaded["revision"] == 1 and reloaded["values"]["enabled"] is True, reloaded
    print("Passed signed temporary host: registry discovery, host settings, provider rejection, CAS and readback.")
    print("Display lifecycle/effects, dynamic session authorization and installed deployment remain unqualified.")


if __name__ == "__main__":
    main()
