#!/usr/bin/env python3
"""Catalog-to-process launch, session leases, ownership and teardown over real HTTP."""
from __future__ import annotations

import json
import os
from pathlib import Path
import sys
import tempfile
import time

from test_component_updates import CARGO, run
from test_host_service import Console, wait_gone
from test_management_api import Api
from test_management_domains import token
from test_platform_modules import PlatformHarness


def post(api, path, payload, bearer):
    status, result, _ = api.post(path, payload, bearer)
    assert status == 200 and result["ok"], result
    return result


def wait_state(api, launch_id, bearer, expected):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        launch = post(api, "/v1/launch/status", {"launch_id": launch_id}, bearer)["launch"]
        if launch["state"] == expected:
            return launch
        assert launch["state"] not in ["failed", "cleanup_failed", "cancelled"], launch
        time.sleep(0.05)
    raise AssertionError(f"launch did not reach {expected}: {launch}")


def create_app(api, bearer, name, seconds, revision, pid_path=None):
    program = f"import time; time.sleep({seconds})"
    if pid_path is not None:
        program = f"import os; from pathlib import Path; Path({str(pid_path)!r}).write_text(str(os.getpid())); " + program
    return post(api, "/v1/catalog/create", {
        "expected_revision": revision,
        "entry": {"name": name, "launch": {
            "kind": "executable", "program": str(Path(sys.executable).resolve()),
            "arguments": ["-c", program],
        }},
    }, bearer)


def lifecycle_command(trace, event, *, delay=0, exit_code=0):
    """A literal argv command with an observable effect and bounded lifetime."""
    program = f"""import sys
import time
from pathlib import Path

time.sleep({delay!r})
with Path({str(trace)!r}).open("a") as output:
    output.write({event + chr(10)!r})
sys.exit({exit_code!r})
"""
    environment = {
        key: os.environ[key]
        for key in ("SystemRoot", "WINDIR")
        if key in os.environ
    }
    return {
        "executable": str(Path(sys.executable).resolve()),
        "args": ["-c", program],
        "environment": environment,
        "timeout_ms": 5000,
    }


def trace_events(path):
    return path.read_text().splitlines() if path.exists() else []


def create_lifecycle_app(api, bearer, directory, revision, *, fail_preparation=False):
    trace = directory / ("failed-preparation.trace" if fail_preparation else "lifecycle.trace")
    preparation = [
        {
            "run": lifecycle_command(trace, "prepare-one"),
            "undo": lifecycle_command(trace, "undo-one"),
        },
        {
            "run": lifecycle_command(
                trace, "prepare-two", delay=0.2,
                exit_code=3 if fail_preparation else 0,
            ),
            "undo": lifecycle_command(trace, "undo-two", delay=0.2),
        },
    ]
    program = (
        "import time; from pathlib import Path; "
        f"Path({str(trace)!r}).open('a').write('application\\n'); time.sleep(30)"
    )
    created = post(api, "/v1/catalog/create", {
        "expected_revision": revision,
        "entry": {
            "name": "Preparation failure" if fail_preparation else "Prepared application",
            "launch": {
                "kind": "executable",
                "program": str(Path(sys.executable).resolve()),
                "arguments": ["-c", program],
                "preparation": preparation,
            },
        },
    }, bearer)
    return created, trace


def test_preparation_and_undo(api, bearer, directory, revision):
    app, trace = create_lifecycle_app(api, bearer, directory, revision)
    request = {"operation_id": "prepared-application", "app_id": app["id"]}
    # Only the canonical catalog can supply preparation commands.
    assert api.post("/v1/launch/start", dict(request, preparation=[]), bearer)[0] == 400
    started = post(api, "/v1/launch/start", request, bearer)["launch"]
    active = wait_state(api, started["launch_id"], bearer, "active")
    deadline = time.monotonic() + 3
    while "application" not in trace_events(trace) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert trace_events(trace) == ["prepare-one", "prepare-two", "application"]
    repeated = post(api, "/v1/launch/start", request, bearer)["launch"]
    assert repeated["launch_id"] == active["launch_id"], repeated
    post(api, "/v1/launch/stop", {"launch_id": active["launch_id"]}, bearer)
    stopped = wait_state(api, active["launch_id"], bearer, "stopped")
    assert stopped["cleanup_error"] is None, stopped
    expected = ["prepare-one", "prepare-two", "application", "undo-two", "undo-one"]
    assert trace_events(trace) == expected, trace_events(trace)
    post(api, "/v1/launch/stop", {"launch_id": active["launch_id"]}, bearer)
    assert trace_events(trace) == expected, "idempotent stop repeated undo"

    failed_app, failed_trace = create_lifecycle_app(
        api, bearer, directory, app["revision"], fail_preparation=True,
    )
    started = post(api, "/v1/launch/start", {
        "operation_id": "preparation-fails", "app_id": failed_app["id"],
    }, bearer)["launch"]
    failed = wait_state(api, started["launch_id"], bearer, "failed")
    assert failed["cleanup_error"] is None, failed
    assert trace_events(failed_trace) == ["prepare-one", "prepare-two", "undo-two", "undo-one"]


def test_workflow(console, api, bearer, directory):
    pid_path = directory / "launched.pid"
    app = create_app(api, bearer, "Controlled sleeper", 30, 0, pid_path)
    request = {"operation_id": "http-launch-one", "app_id": app["id"]}
    started = post(api, "/v1/launch/start", request, bearer)["launch"]
    active = wait_state(api, started["launch_id"], bearer, "active")
    assert active["session_id"] and active["process_id"], active
    deadline = time.monotonic() + 3
    while not pid_path.exists() and time.monotonic() < deadline:
        time.sleep(0.02)
    child_pid = int(pid_path.read_text())
    if os.name == "posix":
        os.kill(child_pid, 0)
    repeated = post(api, "/v1/launch/start", request, bearer)["launch"]
    assert repeated["launch_id"] == active["launch_id"] and repeated["process_id"] == active["process_id"]
    # The management and console principals have distinct catalogs/launch records.
    foreign = console.request("amaterasu.launch/1", {"action": "status", "launch_id": active["launch_id"]})
    assert not foreign["ok"], foreign
    foreign_app = console.request("amaterasu.catalog/1", {"action": "get", "id": app["id"]})
    assert not foreign_app["ok"], foreign_app
    direct = console.request("amaterasu.catalog/1", {"action": "resolve_launch", "id": app["id"]})
    assert not direct["ok"] and direct["error"]["code"] == "permission_denied", direct
    reader = token(console, ["application.read"])
    assert api.post("/v1/launch/start", request, reader)[0] == 403
    assert api.post("/v1/launch/stop", {"launch_id": active["launch_id"]}, reader)[0] == 403
    assert api.post("/v1/launch/start", dict(request, executable="/bin/sh"), bearer)[0] == 400
    post(api, "/v1/launch/stop", {"launch_id": active["launch_id"]}, bearer)
    stopped = wait_state(api, active["launch_id"], bearer, "stopped")
    assert stopped["cleanup_error"] is None, stopped
    if os.name == "posix":
        wait_gone(child_pid)
    assert post(api, "/v1/launch/start", request, bearer)["launch"]["state"] == "stopped"
    # Natural exit must also stop the session and release its app lease.
    short = create_app(api, bearer, "Natural exit", 0.2, app["revision"])
    natural = post(api, "/v1/launch/start", {"operation_id": "natural", "app_id": short["id"]}, bearer)["launch"]
    wait_state(api, natural["launch_id"], bearer, "stopped")
    again = post(api, "/v1/launch/start", {"operation_id": "natural-again", "app_id": short["id"]}, bearer)["launch"]
    wait_state(api, again["launch_id"], bearer, "stopped")
    return short["revision"]


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-launch-") as temporary:
        harness = PlatformHarness(Path(temporary))
        path = harness.root / "bootstrap.json"
        settings = json.loads(path.read_text())
        settings["policy"]["settings"]["amaterasu.management"].update(enabled=True, bind="127.0.0.1:0")
        if os.name == "nt":
            settings["policy"]["settings"]["amaterasu.launch"] = {
                "environment": {key: os.environ[key] for key in ["SystemRoot", "WINDIR"] if key in os.environ},
            }
        path.write_text(json.dumps(settings))
        harness.stage(harness.package(1))
        console = Console(harness)
        try:
            assert console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "test", "password": "local-launch-test-password",
            })["ok"]
            bearer = token(console, ["catalog.read", "catalog.write", "application.read", "application.launch", "application.terminate"])
            api = Api(console)
            revision = test_workflow(console, api, bearer, Path(temporary))
            test_preparation_and_undo(api, bearer, Path(temporary), revision)
            console.eof()
        finally:
            console.close()
    print("Passed: catalog-backed process launch, operation retries, caller/scope isolation, explicit stop, natural exit, ordered preparation, reverse undo, and failed-preparation rollback before lease cleanup.")


if __name__ == "__main__":
    main()
