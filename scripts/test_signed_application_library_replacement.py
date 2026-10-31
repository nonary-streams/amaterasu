#!/usr/bin/env python3
"""Exercise startup-selected catalog-backed replacement in a signed temporary host.

Requires prebuilt Linux session artifacts and independent SDK replacement cdylib.
Uses a private development root and no network listeners, device effects, or
system installation. This read-only fixture does not implement artwork or launch.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import tempfile

from test_linux_session_profile import isolated_environment
from types import SimpleNamespace
from test_signed_application_library_plugins import LIBRARY, PatientConsole, manifest, run

WORKSPACE = Path(__file__).resolve().parents[1]
CATALOG = "amaterasu.catalog/1"
STATE = "amaterasu.state/1"
COMPONENT = "example.library-replacement"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--paired-nvhttp", action="store_true", help="also pair a loopback test client and read /applist over TLS")
    args = parser.parse_args()
    if os.name != "posix":
        raise SystemExit("This qualification uses the Linux session profile")
    artifacts = Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target")).resolve() / "debug"
    publisher, host = artifacts / "amaterasu-release", artifacts / "amaterasu"
    artifact = artifacts / "libexample_application_library_replacement.so"
    with tempfile.TemporaryDirectory(prefix="amaterasu-signed-library-replacement-") as temporary:
        directory = Path(temporary)
        environment = isolated_environment(directory)
        root, package, template = directory / "root", directory / "package", directory / "component.json"
        run(publisher, "init-linux-session", root)
        bootstrap_path = root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        policy = bootstrap["policy"]
        policy["settings"]["amaterasu.rtsp"]["enabled"] = False
        policy["settings"]["amaterasu.nvhttp"]["enabled"] = args.paired_nvhttp
        if args.paired_nvhttp:
            from test_management_api import available_moonlight_port_pair
            port = available_moonlight_port_pair()
            policy["settings"]["amaterasu.nvhttp"]["http_bind"] = f"127.0.0.1:{port}"
            policy["settings"]["amaterasu.nvhttp"]["https_bind"] = f"127.0.0.1:{port - 5}"
            policy["settings"]["amaterasu.nvhttp"]["streaming"]["desktop"]["enabled"] = False
        policy["settings"]["amaterasu.catalog"]["library_principal"] = COMPONENT
        policy["settings"]["amaterasu.catalog"]["export_principals"]["amaterasu.console"] = "amaterasu.console"
        if args.paired_nvhttp:
            policy["settings"]["amaterasu.catalog"]["export_principals"]["amaterasu.nvhttp"] = "amaterasu.console"
        policy["settings"][COMPONENT] = {"allowed_callers": ["amaterasu.console", "amaterasu.nvhttp"],
                                         "catalog_backed": True}
        policy["principals"]["amaterasu.console"].setdefault("call", []).append(LIBRARY)
        component = manifest(COMPONENT, artifact, LIBRARY)
        component["requested"]["call"] = [CATALOG, STATE]
        component["requires"].update({"amaterasu.catalog": "0.1.0", "amaterasu.state": "0.1.0"})
        template.write_text(json.dumps(component))
        bootstrap_path.write_text(json.dumps(bootstrap))
        package_args = [publisher, "pack-linux-session", root, artifacts, package, "1",
                "--external-component", template, artifact, "--replace-library", COMPONENT]
        run(*package_args, fail=True)
        policy["principals"][COMPONENT] = {"provide": [LIBRARY], "call": [CATALOG, STATE]}
        bootstrap_path.write_text(json.dumps(bootstrap))
        run(*package_args)
        run(publisher, "verify", root, package)
        selected = json.loads((package / "manifest.json").read_text())["components"]
        ids = {entry["id"] for entry in selected}
        assert COMPONENT in ids and "amaterasu.library" not in ids, ids
        for entry in selected:
            assert "amaterasu.library" not in entry.get("requires", {}), entry
        run(publisher, "stage", root, package)
        if args.paired_nvhttp:
            from test_nvhttp import install_fixture_identity
            install_fixture_identity(SimpleNamespace(root=root))
        console = PatientConsole(SimpleNamespace(host=host, root=root), environment=environment)
        try:
            created = console.request(CATALOG, {"action": "create", "expected_revision": 0,
                "entry": {"name": "Catalog-backed game", "launch": {"kind": "application", "path": "/fixture/game"}}})
            assert created["ok"], created
            app_id = created["id"]
            listing = console.request(LIBRARY, {"action": "list", "limit": 20})
            assert listing["ok"] and any(row == {"id": app_id, "name": "Catalog-backed game"}
                                         for row in listing["entries"]), listing
            resolved = console.request(LIBRARY, {"action": "resolve", "id": app_id})
            assert resolved == {"ok": True, "revision": created["revision"], "id": app_id,
                                "name": "Catalog-backed game"}, resolved
            forged = console.request(LIBRARY, {"action": "list", "limit": 20,
                                                "initiator": "amaterasu.nvhttp"})
            assert not forged["ok"], forged
            denied = console.request(CATALOG, {"action": "resolve_exported", "id": app_id})
            assert not denied["ok"] and denied["error"]["code"] == "permission_denied", denied
            if args.paired_nvhttp:
                from test_nvhttp import check_library, initialize_host_identity, pair, tls_context, wait_for
                initialize_host_identity(console)
                status = wait_for(console, "amaterasu.nvhttp/1", {"action":"status"}, lambda value: value.get("tls_ready"))
                http, https = "http://" + status["http_address"], "https://" + status["https_address"]
                pair(console, http)
                check_library(https, {str(int(app_id[4:], 16)): "Catalog-backed game"}, tls_context())
            console.eof()
        finally:
            console.close()

    print("Passed: signed startup-selected replacement forwards catalog-backed reads and rejects spoofing.")
    if args.paired_nvhttp:
        print("Passed: paired loopback NVHTTP app listing over TLS through the selected replacement.")
    else:
        print("Paired NVHTTP was not requested.")
    print("Artwork, real game effects, and system installation remain unqualified.")


if __name__ == "__main__":
    main()
