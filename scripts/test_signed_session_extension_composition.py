#!/usr/bin/env python3
"""Isolated, effects-disabled signed startup diagnostic for two v2 providers.

This test deliberately injects the supplied diagnostic lifecycle graph into a
private temporary root. It does not qualify composition, change the public gate,
install a service, or exercise display, capture, input or action effects. Use
only matched, explicitly reviewed test artifacts and loopback listeners.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import tempfile
from unittest import mock

import check_session_extension_composition as check
import prepare_session_extension_composition as prepare
import prepare_session_extension_profile as legacy
import session_extension_composition_policy as composition
from test_session_extension_profile import available_base_port, run


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, epilog=(
        "Diagnostic artifacts can include a test-only coordinator with its v2 "
        "startup gate patched. Record their exact hashes; mixed builds do not "
        "establish ABI or production runtime qualification."))
    parser.add_argument("--enable-display-actions", action="store_true")
    for name in ("publisher", "host", "artifacts", "owner_repo", "owner_library",
                 "display_repo", "display_library", "diagnostic_policy"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    inputs = []
    for repo, library, provider, revision, methods in (
        (args.owner_repo, args.owner_library, check.OWNER, 1, check.OWNER_METHODS),
        (args.display_repo, args.display_library, check.DISPLAY, 2, check.DISPLAY_METHODS),
    ):
        approval = repo / "examples/lifecycle-api-approvals.json"
        digest = hashlib.sha256(approval.read_bytes()).hexdigest()
        inputs.append(prepare.validated_selection(args.publisher, repo, library, provider, revision, methods, digest))
    owner = inputs[0]["selected"]
    policy = json.loads(args.diagnostic_policy.read_text())
    lifecycle = policy["settings"][legacy.LIFECYCLE]
    controls, finalizes = lifecycle["control_policies"], lifecycle["finalize_policies"]
    if len(controls) != 1 or len(finalizes) != 1:
        raise ValueError("diagnostic policy must select one owner graph")
    control, finalize = controls[0], finalizes[0]
    documents = {item["reference"]["id"]: item["document"].encode() for item in lifecycle["schema_documents"]}
    names = {"control": control["owner_method"]["method"], "control_status": control["status_method"]["method"],
             "settlement": control["settlement_method"]["method"], "finalize": finalize["method"]["method"],
             "finalize_status": finalize["status_method"]["method"]}
    graph = composition.lifecycle_graph(owner["approvals"], documents["amaterasu.display-target/1"],
                                         documents["amaterasu.display-control/1"], names)
    if graph != {key: lifecycle[key] for key in ("control_policies", "finalize_policies", "schema_documents")}:
        raise ValueError("diagnostic graph does not match selected pinned composition")
    additional = {provider: lifecycle["operation_grants"][provider] for provider in (check.OWNER, check.DISPLAY)}
    # The startup-only diagnostic never invokes either recovery lookup; their
    # presence in policy does not establish that coordinator routes work.
    additional[check.OWNER] = sorted(set(additional[check.OWNER]) | composition.CORE_OWNER_OPERATIONS)
    if args.enable_display_actions:
        additional[legacy.ACTIONS] = sorted(composition.ACTION_COORDINATOR_OPERATIONS)
        additional[check.OWNER] = sorted(set(additional[check.OWNER]) | composition.ACTION_OWNER_OPERATIONS)
    with tempfile.TemporaryDirectory(prefix="amaterasu-signed-composition-") as temporary:
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
        settings = composition.lifecycle_configuration(str(root / "data/session-lifecycle/journal.json"), graph, additional)
        with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(
                prepare, "validated_runtime_configuration", return_value=settings):
            prepare.pack_profile(args.publisher, root, args.artifacts, package, 1, inputs,
                                 ["--lan", f"127.0.0.1:{available_base_port()}"], args.enable_display_actions)
        run(args.publisher, "stage", root, package, environment=environment)
        def request(payload: dict) -> dict:
            envelope = {"op": "request", "service": legacy.REGISTRY_SERVICE, "payload": payload,
                        "timeout_ms": 8000, "operation_id": "signed-composition-diagnostic"}
            response = json.loads(run(args.host, "probe", root, json.dumps(envelope), environment=environment))
            if response.get("kind") != "result":
                raise AssertionError(response)
            return response["payload"]
        listing = request({"operation": "list", "after": None, "limit": 32, "revision": None})
        if not listing.get("ok") or {item["provider"] for item in listing["items"]} != {check.OWNER, check.DISPLAY}:
            raise AssertionError(listing)
        for provider in (check.OWNER, check.DISPLAY):
            current = request({"operation": "settings_get", "provider": provider, "scope": {"kind": "host"},
                               "application": None, "paired_client": None, "session": None})
            if not current.get("ok") or current["effective"]["enabled"]["value"] is not False:
                raise AssertionError(current)
        bootstrap = json.loads((root / "bootstrap.json").read_text())["policy"]["settings"]
        expected = [check.OWNER + "/remove-owned"] if args.enable_display_actions else []
        if bootstrap[legacy.LIFECYCLE]["input_actions"] != expected:
            raise AssertionError("unexpected action policy")
    print("Passed isolated signed startup, two-provider discovery and disabled settings readback.")
    print("The public runtime gate remains closed; no actions, display effects, capture or hardware were exercised.")
    for item in inputs:
        print(f"{item['report']['provider']} artifact SHA-256 {item['report']['artifact_sha256']}; "
              f"descriptor SHA-256 {item['report']['descriptor_sha256']}; approvals SHA-256 {item['report']['approvals_sha256']}")


if __name__ == "__main__":
    main()
