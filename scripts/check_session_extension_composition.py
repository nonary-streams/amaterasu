#!/usr/bin/env python3
"""Read-only preflight for the two independently built v2 display providers.

This checks selected artifacts and contracts. It neither prepares a bootstrap nor
signs, stages, installs or starts a profile. Runtime composition still requires
approved graph and broker policy plus integrated provider implementations.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

from export_session_extension_split import load

DISPLAY = "nonary.display-settings"
OWNER = "nonary.virtual-screen"
TARGET_DIGEST = "8521cdfd92da98b25136bd01720d397f0d7fee14748c08afde76a0379fa18941"
CONTROL_DIGEST = "c2c3ce866277cb29bbd9fbd947899917538d46b1020df3197fbd264159607d87"
DISPLAY_METHODS = {"settings_validate", "resource_plan", "lifecycle_callback", "lifecycle_status"}
OWNER_METHODS = {"settings_validate", "resource_plan", "lifecycle_callback", "lifecycle_status",
                 "binding_validate", "resource_control", "resource_finalize", "resource_status",
                 "action_plan", "action_execute", "action_status"}


def file_digest(path: Path) -> str:
    if not path.is_file() or path.stat().st_size > 256 * 1024 * 1024:
        raise ValueError("selected artifact is missing or too large")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def check_provider(descriptor: dict, component: dict, approvals: list, provider: str,
                   revision: int, methods: set[str]) -> None:
    service = provider + "/2"
    if descriptor.get("provider") != provider or type(descriptor.get("revision")) is not int or descriptor["revision"] != revision:
        raise ValueError("unexpected provider descriptor identity or revision")
    if component.get("id") != provider or component.get("runtime") is not False:
        raise ValueError("unexpected provider component identity")
    requested = component.get("requested")
    if not isinstance(requested, dict) or requested.get("provide") != [service]:
        raise ValueError("provider must request exactly its selected v2 service")
    if not isinstance(component.get("requires"), dict) or "amaterasu.runtime" not in component["requires"]:
        raise ValueError("provider must declare a runtime dependency")
    method = descriptor.get("settings_validate")
    if method != {"service": service, "method": "settings_validate", "interface_major": 2,
                  "interface_minor": 0, "schema_revision": 1}:
        raise ValueError("settings validation reference does not select v2")
    options = descriptor.get("options")
    if not isinstance(options, list):
        raise ValueError("invalid provider options")
    enabled = [option for option in options if isinstance(option, dict) and option.get("id") == "enabled"]
    if len(enabled) != 1 or enabled[0].get("definition") != {"type": "boolean", "default": False}:
        raise ValueError("provider must start disabled")
    names = []
    for approval in approvals:
        if not isinstance(approval, dict) or approval.get("provider") != provider:
            raise ValueError("approval provider mismatch")
        method = approval.get("method")
        if not isinstance(method, dict) or method.get("service") != service or method.get("interface_major") != 2 or method.get("interface_minor") != 0:
            raise ValueError("approval does not select v2")
        names.append(method.get("method"))
    if len(names) != len(methods) or set(names) != methods:
        raise ValueError("provider method approvals are incomplete, duplicated or unexpected")


def check_schema(path: Path, expected: str) -> str:
    digest = file_digest(path)
    if digest != expected:
        raise ValueError(f"unexpected schema bytes: {path.name}")
    return digest


def check_selected(publisher: Path, repository: Path, library: Path, provider: str,
                   revision: int, methods: set[str]) -> dict:
    examples = repository / "examples"
    descriptor_path = examples / "settings-descriptor.json"
    descriptor_bytes, descriptor = load(descriptor_path)
    component_bytes, component = load(examples / "component-linux.json")
    approvals_bytes, approvals = load(examples / "lifecycle-api-approvals.json")
    if not isinstance(descriptor, dict) or not isinstance(component, dict) or not isinstance(approvals, dict) or set(approvals) != {"api_methods"} or not isinstance(approvals["api_methods"], list):
        raise ValueError("invalid example metadata")
    check_provider(descriptor, component, approvals["api_methods"], provider, revision, methods)
    with tempfile.TemporaryDirectory(prefix="amaterasu-composition-descriptor-") as temporary:
        snapshot = Path(temporary) / "descriptor.json"
        snapshot.write_bytes(descriptor_bytes)
        digest = subprocess.run([str(publisher), "extension-descriptor-digest", str(snapshot)],
                                check=True, capture_output=True, text=True, timeout=30).stdout.strip()
    if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
        raise ValueError("invalid validated descriptor digest")
    return {"provider": provider, "service": provider + "/2", "descriptor_sha256": digest,
            "artifact_sha256": file_digest(library), "component_template_sha256": hashlib.sha256(component_bytes).hexdigest(),
            "approvals_sha256": hashlib.sha256(approvals_bytes).hexdigest(), "methods": sorted(methods)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("publisher", "display_repo", "display_library", "virtual_repo", "virtual_library"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    report = {"providers": [check_selected(args.publisher, args.display_repo, args.display_library,
                                            DISPLAY, 2, DISPLAY_METHODS),
                            check_selected(args.publisher, args.virtual_repo, args.virtual_library,
                                            OWNER, 1, OWNER_METHODS)],
              "schemas": {"display_target": check_schema(args.virtual_repo / "schemas/display-target-v1.json", TARGET_DIGEST),
                          "display_control": check_schema(args.virtual_repo / "schemas/display-control-v1.json", CONTROL_DIGEST)},
              "profile_prepared": False, "profile_signed": False, "runtime_qualified": False}
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
