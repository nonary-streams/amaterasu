#!/usr/bin/env python3
"""Opt-in real PipeWire capture qualification through the desktop portal.

Every run first builds the signed media-probe fixture and proves the PipeWire
module can be loaded, its native ABI bound, and status plus empty enumeration
called without selecting a source. The host portal gate is reported separately.
The script never opens the ScreenCast picker unless the caller explicitly opts
in with ``--select``. The picker remains the normal desktop portal UI and owns
source consent.
The opt-in selection window is limited to 15 seconds so the fixture can clean
up within the development host's 30-second request deadline.
"""
from __future__ import annotations

import argparse
import ast
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console


PORTAL_BUS_NAME = "org.freedesktop.portal.Desktop"
PORTAL_OBJECT = "/org/freedesktop/portal/desktop"
PORTAL_INTERFACE = "org.freedesktop.portal.ScreenCast"
CAPTURE_PRINCIPAL = "amaterasu.media-probe"


def result(status: str, reason: str, **details: object) -> dict[str, object]:
    return {"status": status, "reason": reason, **details}


def preflight() -> dict[str, object] | None:
    """Return an unavailable result, or None when local prerequisites exist.

    NameHasOwner is sent to the bus daemon directly, so checking for the portal
    cannot activate it. Introspection only runs against an already-owned name
    and does not call ScreenCast methods or display a picker.
    """
    if not sys.platform.startswith("linux"):
        return result("unavailable", "linux_required")
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        return result("unavailable", "session_bus_unavailable")

    gdbus = shutil.which("gdbus")
    pkg_config = shutil.which("pkg-config")
    if gdbus is None or pkg_config is None:
        return result("unavailable", "native_dependencies_unavailable")
    dependencies = subprocess.run(
        [pkg_config, "--exists", "libpipewire-0.3", "gio-2.0", "gio-unix-2.0"],
        text=True,
        capture_output=True,
        check=False,
    )
    if dependencies.returncode != 0:
        return result("unavailable", "native_dependencies_unavailable")

    owner = subprocess.run(
        [
            gdbus,
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            PORTAL_BUS_NAME,
        ],
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )
    if owner.returncode != 0:
        return result("unavailable", "session_bus_unavailable")
    if "true" not in owner.stdout.lower():
        return result("unavailable", "portal_unavailable")

    unique_owner = subprocess.run(
        [
            gdbus,
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.GetNameOwner",
            PORTAL_BUS_NAME,
        ],
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )
    if unique_owner.returncode != 0:
        return result("unavailable", "portal_unavailable")
    try:
        parsed_owner = ast.literal_eval(unique_owner.stdout.strip())
    except (SyntaxError, ValueError):
        return result("failed", "portal_owner_response_invalid")
    if (
        not isinstance(parsed_owner, tuple)
        or len(parsed_owner) != 1
        or not isinstance(parsed_owner[0], str)
        or not parsed_owner[0].startswith(":")
    ):
        return result("failed", "portal_owner_response_invalid")
    portal_owner = parsed_owner[0]

    introspection = subprocess.run(
        [
            gdbus,
            "introspect",
            "--session",
            "--dest",
            portal_owner,
            "--object-path",
            PORTAL_OBJECT,
        ],
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )
    if introspection.returncode != 0:
        return result("unavailable", "portal_unavailable")
    if PORTAL_INTERFACE not in introspection.stdout:
        return result("unavailable", "screencast_interface_unavailable")
    return None


def request_capture(console: Console, source_type: str) -> dict[str, object]:
    console.sequence += 1
    console.send(
        {
            "op": "request",
            "service": "test.media-probe/1",
            "payload": {"action": "pipewire_capture", "source_type": source_type},
            # The host currently caps live requests at 30 seconds. Keep the
            # wrapper within that limit; the fixture's longer selection timer
            # cannot be reached through this development console yet.
            "timeout_ms": 30_000,
            "operation_id": f"native-pipewire-{console.sequence}",
        }
    )
    response = console.replies.get(timeout=35)
    assert response is not None, "host exited before PipeWire qualification completed"
    assert response.get("kind") == "result", (response, "".join(console.errors))
    payload = response["payload"]
    qualification = payload.get("qualification")
    if qualification not in {"passed", "unavailable", "skipped", "denied", "failed"}:
        return result("failed", "invalid_fixture_result", fixture=payload)
    return result(qualification, payload.get("code") or "none", fixture=payload)


def request_control_status(console: Console) -> dict[str, object]:
    console.sequence += 1
    console.send(
        {
            "op": "request",
            "service": "test.media-probe/1",
            "payload": {"action": "pipewire_control_status"},
            "timeout_ms": 30_000,
            "operation_id": f"native-pipewire-control-{console.sequence}",
        }
    )
    response = console.replies.get(timeout=35)
    assert response is not None, "host exited before signed PipeWire control proof completed"
    assert response.get("kind") == "result", (response, "".join(console.errors))
    payload = response["payload"]
    if (
        payload.get("control_proof") != "passed"
        or payload.get("native_api_bound") is not True
        or payload.get("status_passed") is not True
        or payload.get("empty_enumeration") is not True
    ):
        return result("failed", "signed_control_proof_failed", control=payload)
    if payload.get("availability") not in {"available", "unavailable"}:
        return result("failed", "invalid_control_availability", control=payload)
    return result(
        "passed",
        "signed_control_proof_passed",
        control={
            "provider_loaded": True,
            "native_api_bound": True,
            "status_passed": True,
            "empty_enumeration": True,
            "native_supported": payload.get("native_supported"),
            "availability": payload["availability"],
            "availability_reason": payload.get("availability_reason"),
            "source_selection": "not_requested",
        },
    )


def run_qualification(source_type: str, select: bool) -> dict[str, object]:
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-native-pipewire-") as temporary:
        harness = ComponentHarness(Path(temporary))
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.capture-pipewire"] = {
            "enabled": True,
            "selection_principals": [CAPTURE_PRINCIPAL],
            "capture_principals": [CAPTURE_PRINCIPAL],
            "selection_timeout_seconds": 15,
        }
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = Path(temporary) / "media-probe"
        run(harness.publisher, "pack-media-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)

        console = Console(harness)
        try:
            control = request_control_status(console)
            if control["status"] != "passed":
                return result(
                    "failed",
                    "signed_control_proof_failed",
                    control=control.get("control"),
                    selection=result("skipped", "control_proof_failed"),
                )
            try:
                unavailable = preflight()
            except (OSError, subprocess.SubprocessError):
                unavailable = result("unavailable", "session_bus_unavailable")
            if unavailable is not None:
                return result(
                    "unavailable",
                    str(unavailable["reason"]),
                    control=control["control"],
                    selection=result("unavailable", str(unavailable["reason"])),
                )
            control_payload = control["control"]
            if control_payload["availability"] != "available":
                reason = control_payload.get("availability_reason") or "portal_unavailable"
                return result(
                    "unavailable",
                    str(reason),
                    control=control_payload,
                    selection=result("unavailable", str(reason)),
                )
            if not select:
                return result(
                    "skipped",
                    "explicit_selection_consent_required",
                    control=control_payload,
                    selection=result("skipped", "explicit_selection_consent_required"),
                )
            selection = request_capture(console, source_type)
            return result(
                str(selection["status"]),
                str(selection["reason"]),
                control=control_payload,
                selection=selection,
            )
        finally:
            console.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source-type",
        choices=("monitor", "window"),
        default="monitor",
        help="which normal portal source category to request (default: monitor)",
    )
    parser.add_argument(
        "--select",
        action="store_true",
        help="open the normal ScreenCast picker (selection window: 15 seconds)",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    try:
        outcome = run_qualification(arguments.source_type, arguments.select)
    except (AssertionError, OSError, subprocess.SubprocessError, TimeoutError) as error:
        outcome = result("failed", "qualification_runner_error", detail=str(error))
    print(json.dumps(outcome, sort_keys=True))
    if outcome["status"] in {"passed", "skipped", "denied", "unavailable"}:
        return 0 if outcome["status"] in {"passed", "skipped"} else 2
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
