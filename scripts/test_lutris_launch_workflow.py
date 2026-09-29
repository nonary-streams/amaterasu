#!/usr/bin/env python3
"""Signed Linux Lutris policy, discovery and canonical-client launch acceptance.

Uses a controlled client and database, not the desktop's Lutris installation.
Every socket listener binds loopback. HOME, XDG_DATA_HOME and PATH are changed
only in the test host child's explicit environment.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

from test_component_updates import CARGO, run
from test_host_service import Console
from test_launch_workflow import post, wait_state
from test_management_api import Api
from test_management_domains import token
from test_platform_modules import PlatformHarness, configure_steam_fixture

GAME_NAME = "Isolated Lutris launch"
GAME_ID = 42


class FixtureAbort(Exception):
    """Controlled assertion-abort analogue used to exercise owned cleanup."""


def create_source(directory):
    home = directory / "isolated-home"
    data = directory / "isolated-data"
    client_directory = directory / "isolated-bin"
    for path in [home, data, client_directory, data / "lutris"]:
        path.mkdir(mode=0o700)
    database = data / "lutris/pga.db"
    with sqlite3.connect(database) as connection:
        connection.execute("""CREATE TABLE games (
            id INTEGER, name TEXT, slug TEXT, runner TEXT, platform TEXT,
            directory TEXT, configpath TEXT, service TEXT, service_id TEXT,
            installed INTEGER
        )""")
        connection.execute("INSERT INTO games VALUES (?,?,?,?,?,?,?,?,?,?)", (
            GAME_ID, GAME_NAME, "isolated-launch", "wine", "linux", "",
            "isolated-game.yml", "", "", 1,
        ))
    database.chmod(0o600)
    marker = directory / "client-launch.json"
    client = client_directory / "lutris"
    interpreter = Path(sys.executable).resolve()
    client.write_text(f"""#!{interpreter}
import json
import os
from pathlib import Path
import sys
import time

marker = Path({str(marker)!r})
partial = marker.with_suffix(".partial")
partial.write_text(json.dumps({{
    "pid": os.getpid(), "program": str(Path(sys.argv[0]).resolve()),
    "arguments": sys.argv[1:], "cwd": os.getcwd(),
}}))
partial.replace(marker)
time.sleep(60)
""")
    client.chmod(0o700)
    environment = os.environ.copy()
    environment.update(HOME=str(home), XDG_DATA_HOME=str(data), PATH=str(client_directory))
    return client.resolve(), marker, environment


def ready(console, api, bearer):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        document = console.request("amaterasu.management/1", {"action": "product_get"})
        if document.get("ok"):
            status = post(api, "/v1/lutris/status", {}, bearer)
            if status["settings_ready"]:
                return status
        time.sleep(0.025)
    raise AssertionError("management and Lutris settings did not load")


def imported(api, bearer, client):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        status = post(api, "/v1/lutris/status", {}, bearer)
        page = post(api, "/v1/catalog/list", {"limit": 32}, bearer)
        rows = [row for row in page["entries"] if row["entry"]["name"] == GAME_NAME]
        if rows and not status["refresh_pending"]:
            assert len(rows) == 1, rows
            launch = rows[0]["entry"]["launch"]
            assert launch["program"] == str(client), launch
            assert launch["arguments"] == [f"lutris:rungameid/{GAME_ID}"], launch
            assert launch["auto_detach"] is True and launch["wait_all"] is False, launch
            assert status["last_error"] is None and status["game_count"] == 1, status
            return rows[0]
        time.sleep(0.04)
    raise AssertionError(f"isolated Lutris source did not reconcile: {status}, {page}")


def save_enabled(api, bearer, enabled):
    current = post(api, "/v1/lutris/settings", {}, bearer)
    changed = post(api, "/v1/lutris/settings_update", {
        "enabled": enabled, "auto_sync": False,
        "expected_revision": current["revision"],
    }, bearer)
    assert changed["revision"] == current["revision"] + 1, changed
    assert changed["values"]["enabled"] is enabled, changed
    confirmed = post(api, "/v1/lutris/settings", {}, bearer)
    assert confirmed == changed, (confirmed, changed)
    return confirmed


def deny_disabled(api, bearer, row, marker, operation):
    assert post(api, "/v1/catalog/get", {"id": row["id"]}, bearer)["entry"] == row["entry"]
    status = post(api, "/v1/lutris/status", {}, bearer)
    assert status["settings_ready"] and not status["enabled"], status
    started = post(api, "/v1/launch/start", {"operation_id": operation, "app_id": row["id"]}, bearer)["launch"]
    failed = wait_state(api, started["launch_id"], bearer, "failed")
    assert failed["failure"] == "lutris_unavailable", failed
    assert failed["process_id"] is None and failed["session_id"] is None, failed
    assert failed["cleanup_error"] is None and not marker.exists(), failed


def owned_client(marker, client):
    deadline = time.monotonic() + 5
    while not marker.exists() and time.monotonic() < deadline:
        time.sleep(0.025)
    assert marker.exists(), "admitted canonical client never ran"
    observed = json.loads(marker.read_text())
    assert observed["program"] == str(client), observed
    assert observed["arguments"] == [f"lutris:rungameid/{GAME_ID}"], observed
    pid = observed["pid"]
    process = Path(f"/proc/{pid}")
    before = (process / "stat").read_text().rsplit(")", 1)[1].split()[19]
    descriptor = os.pidfd_open(pid)
    try:
        assert process.stat().st_uid == os.geteuid()
        assert (process / "exe").resolve() == Path(sys.executable).resolve()
        arguments = (process / "cmdline").read_bytes().split(b"\0")
        assert arguments[1:3] == [os.fsencode(client), f"lutris:rungameid/{GAME_ID}".encode()], arguments
        assert (process / "stat").read_text().rsplit(")", 1)[1].split()[19] == before
        return pid, descriptor
    except BaseException:
        os.close(descriptor)
        raise


def wait_client_exit(descriptor):
    polling = select.poll()
    polling.register(descriptor, select.POLLIN)
    events = polling.poll(3000)
    assert any(ready == descriptor and event & select.POLLIN for ready, event in events), "verified client did not exit within its cleanup bound"


def terminate_owned_client(descriptor):
    try:
        signal.pidfd_send_signal(descriptor, signal.SIGKILL)
    except ProcessLookupError:
        pass
    wait_client_exit(descriptor)


def cleanup_owned_clients(clients):
    failure = None
    for _, descriptor in clients:
        try:
            terminate_owned_client(descriptor)
        except BaseException as error:
            # A failure for one retained identity must not abandon other handles.
            if failure is None:
                failure = error
        finally:
            os.close(descriptor)
    if failure is not None:
        raise failure


def abort_after_admission(api, bearer, row, client, marker, unrelated, owned_clients):
    started = post(api, "/v1/launch/start", {
        "operation_id": "controlled-abort", "app_id": row["id"],
    }, bearer)["launch"]
    active = wait_state(api, started["launch_id"], bearer, "active")
    pid, descriptor = owned_client(marker, client)
    owned_clients.append((pid, descriptor))
    try:
        try:
            raise FixtureAbort("controlled failure after client admission")
        finally:
            terminate_owned_client(descriptor)
    except FixtureAbort:
        pass
    stopped = wait_state(api, active["launch_id"], bearer, "stopped")
    assert stopped["cleanup_error"] is None and unrelated.poll() is None, stopped
    marker.unlink()


def launch_and_stop(api, bearer, row, client, marker, operation, unrelated, owned_clients):
    request = {"operation_id": operation, "app_id": row["id"]}
    started = post(api, "/v1/launch/start", request, bearer)["launch"]
    active = wait_state(api, started["launch_id"], bearer, "active")
    pid, descriptor = owned_client(marker, client)
    owned_clients.append((pid, descriptor))
    assert active["executable_path"] == str(client) and active["process_id"] is not None, active
    retry = post(api, "/v1/launch/start", request, bearer)["launch"]
    assert retry["launch_id"] == active["launch_id"] and retry["process_id"] == active["process_id"], retry
    post(api, "/v1/launch/stop", {"launch_id": active["launch_id"]}, bearer)
    stopped = wait_state(api, active["launch_id"], bearer, "stopped")
    assert stopped["cleanup_error"] is None, stopped
    wait_client_exit(descriptor)
    assert unrelated.poll() is None, "application stop signaled an unrelated process"
    marker.unlink()
    return stopped


def main():
    if not sys.platform.startswith("linux") or os.geteuid() == 0:
        raise RuntimeError("this acceptance requires a non-root current-user Linux host")
    if "--skip-build" not in sys.argv[1:]:
        run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-lutris-launch-") as temporary:
        directory = Path(temporary)
        client, marker, environment = create_source(directory)
        harness = PlatformHarness(directory)
        configure_steam_fixture(harness)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.management"].update(enabled=True, bind="127.0.0.1:0")
        # This test uses ordinary application launch, without public streaming listeners.
        bootstrap["policy"]["settings"]["amaterasu.nvhttp"]["enabled"] = False
        bootstrap_path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        unrelated = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(90)"], env=environment)
        console = None
        owned_clients = []
        try:
            console = Console(harness, environment=environment)
            setup = console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "lutris-test", "password": "isolated-lutris-test-password",
            })
            assert setup["ok"], setup
            bearer = token(console, ["configuration.read", "configuration.write", "catalog.read",
                                     "application.read", "application.launch", "application.terminate"])
            api = Api(console)
            status = ready(console, api, bearer)
            assert status["platform_supported"] and status["available"], status
            # Freeze automatic discovery after the initial import so a missing fixture
            # cannot cause a system Lutris fallback to replace its canonical row.
            save_enabled(api, bearer, True)
            row = imported(api, bearer, client)
            disabled = save_enabled(api, bearer, False)
            deny_disabled(api, bearer, row, marker, "disabled-before-restart")
            console.eof()
            console = Console(harness, environment=environment)
            api = Api(console)
            ready(console, api, bearer)
            assert post(api, "/v1/lutris/settings", {}, bearer) == disabled
            deny_disabled(api, bearer, row, marker, "disabled-after-restart")

            save_enabled(api, bearer, True)
            row = imported(api, bearer, client)
            stopped = launch_and_stop(api, bearer, row, client, marker, "enabled-client", unrelated, owned_clients)
            # A new operation must reacquire the same app's exclusive lease. This
            # proves release through its owning Launch service, without bypassing
            # the Session principal boundary to inspect someone else's session.
            relaunched = launch_and_stop(api, bearer, row, client, marker, "after-stop", unrelated, owned_clients)
            assert relaunched["process_id"] != stopped["process_id"], (stopped, relaunched)
            abort_after_admission(api, bearer, row, client, marker, unrelated, owned_clients)

            client.rename(client.with_name("lutris-unavailable"))
            status = post(api, "/v1/lutris/status", {}, bearer)
            fallback_available = status["available"]
            assert status["settings_ready"] and status["enabled"] and status["database_available"], status
            assert post(api, "/v1/catalog/get", {"id": row["id"]}, bearer)["entry"]["launch"]["program"] == str(client)
            started = post(api, "/v1/launch/start", {"operation_id": "missing-client", "app_id": row["id"]}, bearer)["launch"]
            failed = wait_state(api, started["launch_id"], bearer, "failed")
            assert failed["failure"] == ("application_launch_failed" if status["available"] else "lutris_unavailable"), (status, failed)
            assert failed["cleanup_error"] is None and not marker.exists(), failed
            assert unrelated.poll() is None, "failed launch signaled an unrelated process"
            client.with_name("lutris-unavailable").rename(client)
            launch_and_stop(api, bearer, row, client, marker, "after-missing-client", unrelated, owned_clients)
            console.eof()
        finally:
            try:
                cleanup_owned_clients(owned_clients)
            finally:
                try:
                    if console is not None:
                        try:
                            if console.process.poll() is None and not console.process.stdin.closed:
                                console.eof()
                        finally:
                            console.close()
                finally:
                    if unrelated.poll() is None:
                        unrelated.kill()
                    unrelated.wait(timeout=5)
    print(f"Passed: signed Linux Lutris import, durable disable/restart launch rejection, canonical URI argv and owned client stop, exclusive lease reacquisition, pidfd abort cleanup, missing-client rejection (fallback pair available: {fallback_available}) and unrelated process survival. Controlled client; real Lutris/Wine/Flatpak runner delivery remains unqualified.")


if __name__ == "__main__":
    main()
