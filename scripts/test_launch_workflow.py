#!/usr/bin/env python3
"""Catalog-to-process and process-less Desktop launch, session leases, ownership and teardown
over real loopback HTTP. Every command and marker lives in a temporary directory; no host
display, audio or input state is touched."""
from __future__ import annotations

import json
import os
from pathlib import Path
import random
import sys
import tempfile
import time

from test_component_updates import CARGO, run
from test_host_service import Console, wait_gone
from test_management_api import Api
from test_management_domains import token
from test_platform_modules import PlatformHarness


def post(api, path, payload, bearer):
    # The launch provider runs one dependency workflow at a time, and its idle
    # monitor can briefly hold it; workflow_busy is documented and retryable.
    for _ in range(200):
        status, result, _ = api.post(path, payload, bearer)
        if not (status == 200 and result.get("code") == "workflow_busy"):
            break
        # Jitter keeps retries from phase-locking with the provider's tick.
        time.sleep(random.uniform(0.02, 0.15))
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


def desktop_steps(trace, *, fail_preparation=False):
    return [
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


def call(console, service, payload, timeout_ms=10000):
    """One signed-module request through the host console principal.

    The management HTTP surface validates catalog launch kinds itself and does
    not yet admit Desktop, so this composition drives catalog, launch, session
    and application directly. The launch provider serves one workflow at a time,
    so workflow_busy is retried with jitter.
    """
    reply = None
    for _ in range(200):
        console.sequence += 1
        console.send({
            "op": "request", "service": service, "payload": payload,
            "timeout_ms": timeout_ms, "operation_id": f"desktop-launch.{console.sequence}",
        })
        result = console.receive()
        assert result["kind"] == "result", result
        reply = result["payload"]
        if reply.get("code") != "workflow_busy":
            return reply
        time.sleep(random.uniform(0.02, 0.15))
    return reply


def accepted(reply):
    assert reply["ok"], reply
    return reply


def create_desktop(console, revision, name, preparation, **launch):
    return accepted(call(console, "amaterasu.catalog/1", {
        "action": "create", "expected_revision": revision,
        "entry": {"name": name, "launch": {"kind": "desktop", "preparation": preparation, **launch}},
    }))


def launch_state(console, launch_id, expected):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        launch = accepted(call(console, "amaterasu.launch/1", {
            "action": "status", "launch_id": launch_id,
        }))["launch"]
        if launch["state"] == expected:
            return launch
        assert launch["state"] not in ["failed", "cleanup_failed", "cancelled"], launch
        time.sleep(0.05)
    raise AssertionError(f"launch did not reach {expected}: {launch}")


def start(console, operation_id, app_id):
    return accepted(call(console, "amaterasu.launch/1", {
        "action": "launch", "operation_id": operation_id, "app_id": app_id,
    }))["launch"]


def stop(console, launch_id):
    return accepted(call(console, "amaterasu.launch/1", {
        "action": "stop", "launch_id": launch_id,
    }))["launch"]


def wait_trace(trace, expected):
    deadline = time.monotonic() + 5
    while trace_events(trace) != expected and time.monotonic() < deadline:
        time.sleep(0.02)
    assert trace_events(trace) == expected, trace_events(trace)


def test_desktop_launch(console, directory):
    """A process-less Desktop session: prepare, activate, stop, undo in reverse."""
    # The Desktop kind names nothing to run: every process or game identity is rejected.
    for forbidden in (
        {"program": str(Path(sys.executable).resolve())},
        {"path": "/usr/bin/true"},
        {"game_id": "a1b2c3d4-1234-5678-9abc-0123456789ab"},
        {"steam_id": "480"},
        {"elevated": True},
        {"unknown": 1},
    ):
        reply = call(console, "amaterasu.catalog/1", {
            "action": "create", "expected_revision": 0,
            "entry": {"name": "Forbidden", "launch": {"kind": "desktop", **forbidden}},
        })
        assert not reply["ok"], (forbidden, reply)
    reply = call(console, "amaterasu.catalog/1", {
        "action": "create", "expected_revision": 0,
        "entry": {"name": "Elevated", "elevated": True, "launch": {"kind": "desktop"}},
    })
    assert not reply["ok"] and reply["error"]["code"] == "invalid_entry", reply

    trace = directory / "desktop.trace"
    desktop = create_desktop(console, 0, "Desktop", desktop_steps(trace))
    assert desktop["entry"]["launch"]["kind"] == "desktop", desktop
    assert desktop["entry"]["launch"]["exit_timeout_seconds"] == 10, desktop
    started = start(console, "desktop-session", desktop["id"])
    active = launch_state(console, started["launch_id"], "active")
    assert active["session_id"] and active["process_id"] is None, active
    assert "executable_path" not in active, active
    # The session activates only after preparation completed, and nothing else ran.
    assert trace_events(trace) == ["prepare-one", "prepare-two"], trace_events(trace)
    repeated = start(console, "desktop-session", desktop["id"])
    assert repeated["launch_id"] == active["launch_id"], repeated
    assert trace_events(trace) == ["prepare-one", "prepare-two"], "retry repeated preparation"

    # The same operation cannot move to another entry, and the held lease keeps
    # a second operation from preparing the same entry again.
    other = create_desktop(console, desktop["revision"], "Second desktop", [])
    conflict = call(console, "amaterasu.launch/1", {
        "action": "launch", "operation_id": "desktop-session", "app_id": other["id"],
    })
    assert not conflict["ok"] and conflict["code"] == "operation_conflict", conflict
    contender = start(console, "desktop-contender", desktop["id"])
    assert contender["state"] != "active", contender
    assert launch_state(console, active["launch_id"], "active")["state"] == "active"
    assert trace_events(trace) == ["prepare-one", "prepare-two"], "contender prepared"

    stop(console, active["launch_id"])
    stopped = launch_state(console, active["launch_id"], "stopped")
    assert stopped["cleanup_error"] is None and stopped["process_id"] is None, stopped
    expected = ["prepare-one", "prepare-two", "undo-two", "undo-one"]
    wait_trace(trace, expected)
    stop(console, active["launch_id"])
    assert trace_events(trace) == expected, "idempotent stop repeated undo"

    # An entry without preparation needs no application lease at all.
    bare = start(console, "desktop-bare", other["id"])
    launch_state(console, bare["launch_id"], "active")
    stop(console, bare["launch_id"])
    launch_state(console, bare["launch_id"], "stopped")

    # Failed preparation rolls back in reverse and releases the lease: the same
    # entry launches again once its plan is repaired.
    failing_trace = directory / "desktop-failing.trace"
    failing = create_desktop(
        console, other["revision"], "Failing desktop",
        desktop_steps(failing_trace, fail_preparation=True),
    )
    attempt = start(console, "desktop-fails", failing["id"])
    failed = launch_state(console, attempt["launch_id"], "failed")
    assert failed["cleanup_error"] is None and failed["process_id"] is None, failed
    wait_trace(failing_trace, ["prepare-one", "prepare-two", "undo-two", "undo-one"])
    repaired_trace = directory / "desktop-repaired.trace"
    accepted(call(console, "amaterasu.catalog/1", {
        "action": "update", "id": failing["id"], "expected_revision": failing["revision"],
        "entry": {"name": "Failing desktop", "launch": {
            "kind": "desktop", "preparation": desktop_steps(repaired_trace),
        }},
    }))
    retried = start(console, "desktop-repaired", failing["id"])
    launch_state(console, retried["launch_id"], "active")
    stop(console, retried["launch_id"])
    launch_state(console, retried["launch_id"], "stopped")
    wait_trace(repaired_trace, ["prepare-one", "prepare-two", "undo-two", "undo-one"])

    # Leave the console catalog as found: later checks rely on it being empty.
    for entry in (desktop, other, failing):
        revision = accepted(call(console, "amaterasu.catalog/1", {"action": "list"}))["revision"]
        accepted(call(console, "amaterasu.catalog/1", {
            "action": "delete", "id": entry["id"], "expected_revision": revision,
        }))


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


def isolated_environment(directory):
    """Keep the host from importing the real user's Lutris or Steam libraries.

    Provider imports race the test's catalog revisions; an empty home also keeps
    the run from reading any user data.
    """
    environment = os.environ.copy()
    for name in ("home", "data", "config", "state", "cache"):
        (directory / f"isolated-{name}").mkdir(mode=0o700, exist_ok=True)
    environment.update(
        HOME=str(directory / "isolated-home"),
        XDG_DATA_HOME=str(directory / "isolated-data"),
        XDG_CONFIG_HOME=str(directory / "isolated-config"),
        XDG_STATE_HOME=str(directory / "isolated-state"),
        XDG_CACHE_HOME=str(directory / "isolated-cache"),
    )
    return environment


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
        console = Console(harness, environment=isolated_environment(Path(temporary)))
        try:
            assert console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "test", "password": "local-launch-test-password",
            })["ok"]
            bearer = token(console, ["catalog.read", "catalog.write", "application.read", "application.launch", "application.terminate"])
            api = Api(console)
            test_desktop_launch(console, Path(temporary))
            print("Passed: process-less Desktop composition (catalog, launch, session, application prepare lease).", flush=True)
            revision = test_workflow(console, api, bearer, Path(temporary))
            test_preparation_and_undo(api, bearer, Path(temporary), revision)
            console.eof()
        finally:
            console.close()
    print("Passed: catalog-backed process launch, operation retries, caller/scope isolation, explicit stop, natural exit, ordered preparation, reverse undo, failed-preparation rollback before lease cleanup, and process-less Desktop sessions with preparation, reverse undo, retry idempotency and lease release.")


if __name__ == "__main__":
    main()
