#!/usr/bin/env python3
"""Subprocess tests of the persistent development host with real signed modules."""
from __future__ import annotations

import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time

from test_component_updates import CARGO, run
from test_platform_modules import PlatformHarness


class Console:
    def __init__(self, harness: PlatformHarness):
        self.process = subprocess.Popen(
            [str(harness.host), "serve", str(harness.root)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, bufsize=1,
        )
        self.replies = queue.Queue()
        self.errors = []
        self.sequence = 0
        threading.Thread(target=self.read_replies, daemon=True).start()
        threading.Thread(target=self.read_errors, daemon=True).start()

    def read_replies(self):
        for line in self.process.stdout:
            self.replies.put(json.loads(line))
        self.replies.put(None)

    def read_errors(self):
        self.errors.extend(self.process.stderr)

    def send(self, command):
        self.process.stdin.write(json.dumps(command) + "\n")
        self.process.stdin.flush()

    def receive(self):
        reply = self.replies.get(timeout=15)
        assert reply is not None, "".join(self.errors)
        return reply

    def request(self, service, payload):
        self.sequence += 1
        self.send({"op": "request", "service": service, "payload": payload,
                   "timeout_ms": 2000, "operation_id": f"host-test.{self.sequence}"})
        result = self.receive()
        assert result["kind"] == "result", result
        return result["payload"]

    def eof(self):
        self.process.stdin.close()
        assert self.process.wait(timeout=8) == 0, "".join(self.errors)

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=8)


def worker_pid(console):
    for _ in range(100):
        pids = [int(line.strip().split("pid=")[1]) for line in console.errors
                if line.startswith("Development worker ready: pid=")]
        if pids:
            return pids[-1]
        time.sleep(0.02)
    raise AssertionError("worker never reported readiness")


def wait_gone(pid, timeout=2):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.02)
    raise AssertionError(f"process {pid} survived host shutdown")


def test_live_workflows(harness):
    console = Console(harness)
    try:
        state = console.request("amaterasu.state/1", {"action": "set", "key": "host-live", "value": 7})
        assert state["ok"], state
        session = console.request("amaterasu.session/1", {"action": "create", "key": "live-client"})
        assert session["ok"], session
        time.sleep(0.2)
        snapshot = console.request("amaterasu.session/1", {"action": "snapshot", "session_id": 1})
        assert snapshot["sessions"][0]["key"] == "live-client", snapshot
        console.send({"nonsense": True})
        assert console.receive()["code"] == "invalid_request"
        restored = console.request("amaterasu.state/1", {"action": "get", "key": "host-live"})
        assert restored["value"] == 7, restored
        launched = console.request("amaterasu.application/1", {
            "action": "launch", "operation_id": "host-test.sleeper",
            "executable": str(Path(sys.executable).resolve()),
            "args": ["-c", "import time; time.sleep(60)"],
            "environment": {key: os.environ[key] for key in ("SYSTEMROOT", "WINDIR") if key in os.environ},
        })
        assert launched["ok"], launched
        child_pid = launched["process"]["pid"]
        host_worker = worker_pid(console) if os.name == "posix" else None
        console.eof()
        if os.name == "posix":
            wait_gone(child_pid)
            wait_gone(host_worker)
    finally:
        console.close()

    console = Console(harness)
    try:
        restored = console.request("amaterasu.state/1", {"action": "get", "key": "host-live"})
        assert restored["value"] == 7, restored
        console.send({"host": "shutdown"})
        assert console.receive() == {"kind": "ok"}
        assert console.process.wait(timeout=8) == 0
    finally:
        console.close()


def test_crash_does_not_replay(harness):
    if os.name != "posix":
        return
    console = Console(harness)
    try:
        console.request("amaterasu.state/1", {"action": "get", "key": "never-replayed"})
        old_worker = worker_pid(console)
        os.kill(old_worker, signal.SIGSTOP)
        console.send({"op": "request", "service": "amaterasu.state/1",
                      "payload": {"action": "set", "key": "never-replayed", "value": 99},
                      "timeout_ms": 2000, "operation_id": "host-test.ambiguous"})
        time.sleep(0.3)  # Supervisor dispatched into the stopped worker's pipe.
        os.kill(old_worker, signal.SIGKILL)
        failed = console.receive()
        assert failed["kind"] == "error" and failed["code"] == "worker_unavailable", failed
        missing = console.request("amaterasu.state/1", {"action": "get", "key": "never-replayed"})
        assert not missing["found"], missing
        assert worker_pid(console) != old_worker
        console.eof()
    finally:
        console.close()


def test_idle_watchdog_and_restart_budget(harness):
    if os.name != "posix":
        return
    console = Console(harness)
    try:
        console.request("amaterasu.state/1", {"action": "get", "key": "host-live"})
        original = worker_pid(console)
        os.kill(original, signal.SIGSTOP)
        # Check the five-second liveness watchdog independently of loading the
        # replacement's signed artifacts and native modules. READY measures both.
        wait_gone(original, timeout=7)
        deadline = time.monotonic() + 15
        while worker_pid(console) == original and time.monotonic() < deadline:
            time.sleep(0.1)
        assert worker_pid(console) != original, (
            "replacement worker did not become ready: " + "".join(console.errors)
        )
        # The watchdog consumed one of three lifetime restarts. Three more crashes
        # must exhaust the budget, even though workers answer successfully between.
        for crash in range(3):
            console.request("amaterasu.state/1", {"action": "get", "key": "host-live"})
            victim = worker_pid(console)
            os.kill(victim, signal.SIGSTOP)
            console.send({"op": "request", "service": "amaterasu.state/1",
                          "payload": {"action": "get", "key": "host-live"},
                          "timeout_ms": 2000, "operation_id": f"host-test.crash.{crash}"})
            time.sleep(0.2)
            os.kill(victim, signal.SIGKILL)
            assert console.receive()["code"] == "worker_unavailable"
        assert console.process.wait(timeout=8) != 0
        assert any("restart budget exhausted" in line for line in console.errors)
    finally:
        console.close()


def test_stopped_worker_large_pipe_write_is_bounded(harness):
    if os.name != "posix":
        return
    console = Console(harness)
    try:
        console.request("amaterasu.state/1", {"action": "get", "key": "large-never-replayed"})
        victim = worker_pid(console)
        os.kill(victim, signal.SIGSTOP)
        # This valid contract line exceeds typical pipe capacity. Delivery must
        # not prevent the supervisor's heartbeat watchdog from running.
        console.send({"op": "request", "service": "amaterasu.state/1",
                      "payload": {"action": "set", "key": "large-never-replayed", "value": "x" * 64000},
                      "timeout_ms": 2000, "operation_id": "host-test.large-stalled"})
        failed = console.receive()
        assert failed["code"] == "worker_unavailable", failed
        wait_gone(victim)
        missing = console.request("amaterasu.state/1", {"action": "get", "key": "large-never-replayed"})
        assert not missing["found"], missing
        console.eof()
    finally:
        console.close()


def test_oversized_input_shuts_down(harness):
    console = Console(harness)
    try:
        console.process.stdin.write("x" * 65_537 + "\n")
        console.process.stdin.flush()
        assert console.receive()["code"] == "invalid_request"
        assert console.process.wait(timeout=8) != 0
    finally:
        console.close()


def test_service_mode_holds_root_until_controlled_stop(harness):
    process = subprocess.Popen(
        [str(harness.host), "service", str(harness.root)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True,
    )
    try:
        time.sleep(0.5)
        assert process.poll() is None, process.stderr.read()
        replacement = harness.package(2)
        run(harness.publisher, "stage", harness.root, replacement, expect_success=False)
        process.stdin.close()
        assert process.wait(timeout=8) == 0, process.stderr.read()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=8)


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-host-") as temporary:
        harness = PlatformHarness(Path(temporary))
        harness.stage(harness.package(1))
        test_live_workflows(harness)
        test_crash_does_not_replay(harness)
        test_idle_watchdog_and_restart_budget(harness)
        test_stopped_worker_large_pipe_write_is_bounded(harness)
        test_oversized_input_shuts_down(harness)
        test_service_mode_holds_root_until_controlled_stop(harness)
    print("Passed: persistent host workflows, durable restart, invalid input isolation,")
    print("EOF child cleanup, SCM-compatible service EOF stop, explicit shutdown, crash recovery without mutation replay, watchdog/restart budget, bounded input.")


if __name__ == "__main__":
    main()
