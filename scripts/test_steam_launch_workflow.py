#!/usr/bin/env python3
"""Signed Linux Steam import and real kernel game association with owned fixtures.

This uses controlled Steam/game executables, not an installed Steam or Proton
session. Every listener is ephemeral loopback; fixture environments are passed
only to the host child. No desktop process or service is signaled.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

from test_component_updates import CARGO, run
from test_host_service import Console
from test_launch_workflow import post, wait_state
from test_management_api import Api, solid_portrait_png, stop_console
from test_management_domains import token
from test_platform_modules import PlatformHarness


class OwnedProcesses:
    """Only same-user, start-time/exe/argv-verified fixture identities may be killed."""

    def __init__(self):
        self.identities = []
        self.remembered = set()
        self.allowed = {}
        self.children = []

    def allow(self, executable, arguments, cwd, priority):
        identity = (executable.resolve(), tuple(os.fsencode(value) for value in arguments))
        directories, known_priority = self.allowed.setdefault(identity, (set(), priority))
        assert known_priority == priority, "fixture cleanup priority changed"
        directories.add(cwd.resolve())

    def remember(self, pid, executable, arguments, priority, parent=None, start=None):
        process = Path(f"/proc/{pid}")
        initial = (process / "stat").read_text().rsplit(")", 1)[1].split()
        before = initial[19]
        assert start is None or before == start, "fixture start time changed"
        descriptor = os.pidfd_open(pid)
        try:
            assert process.stat().st_uid == os.geteuid(), "fixture process changed user"
            assert (process / "exe").resolve() == executable.resolve(), "fixture executable changed"
            observed = (process / "cmdline").read_bytes().split(b"\0")[:-1]
            assert observed == [os.fsencode(value) for value in arguments], observed
            if parent is not None:
                assert int(initial[1]) == parent, "fixture parent identity changed"
            assert (process / "stat").read_text().rsplit(")", 1)[1].split()[19] == before, "fixture identity changed"
        except BaseException:
            os.close(descriptor)
            raise
        self.identities.append((priority, descriptor))
        self.remembered.add((pid, before))
        return descriptor

    def start(self, arguments, executable, environment, cwd, priority=4):
        self.allow(executable, arguments, cwd, priority)
        child = subprocess.Popen([str(value) for value in arguments], env=environment, cwd=cwd)
        self.children.append((child, None))
        descriptor = self.remember(child.pid, executable, arguments, priority, os.getpid())
        self.children[-1] = (child, descriptor)
        return descriptor

    @staticmethod
    def exited(descriptor, timeout_ms=0):
        polling = select.poll()
        polling.register(descriptor, select.POLLIN)
        events = polling.poll(timeout_ms)
        assert all(not event & (select.POLLERR | select.POLLNVAL) for _, event in events), "fixture pidfd became invalid"
        return any(ready == descriptor and event & select.POLLIN for ready, event in events)

    def assert_alive(self, descriptors):
        assert all(not self.exited(descriptor) for descriptor in descriptors), "stop affected a protected fixture"

    def recover(self):
        """Recover only exact private fixture vectors, including pre-marker launches."""
        recovered = 0
        entries = list(Path("/proc").iterdir())
        assert len(entries) <= 100000, "fixture recovery process enumeration exceeded its bound"
        for process in entries:
            if not process.name.isdecimal():
                continue
            try:
                if process.stat().st_uid != os.geteuid():
                    continue
                executable = (process / "exe").resolve(strict=True)
                if not any(executable == candidate[0] for candidate in self.allowed):
                    continue
                arguments = tuple((process / "cmdline").read_bytes().split(b"\0")[:-1])
                policy = self.allowed.get((executable, arguments))
                if policy is None or (process / "cwd").resolve(strict=True) not in policy[0]:
                    continue
                start = (process / "stat").read_text().rsplit(")", 1)[1].split()[19]
                pid = int(process.name)
                if (pid, start) not in self.remembered:
                    self.remember(pid, executable, arguments, policy[1], start=start)
                    recovered += 1
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        return recovered

    def cleanup(self, preserve=()):
        failure = None
        preserved = set(preserve)
        deadline = time.monotonic() + 25
        settled = False
        try:
            # Host admission ends before recovery. A helper may still finish a
            # pending spawn, so repeat exact-vector recovery until no fixture
            # remains beyond the intentionally preserved baseline processes.
            while time.monotonic() < deadline:
                self.recover()
                pending = [descriptor for _, descriptor in sorted(self.identities)
                           if descriptor not in preserved and not self.exited(descriptor)]
                if not pending:
                    time.sleep(0.05)
                    if self.recover() == 0:
                        settled = True
                        break
                for descriptor in pending:
                    try:
                        signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    if not self.exited(descriptor, 2000):
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        assert self.exited(descriptor, 3000), "owned fixture cleanup did not settle"
            assert settled, "fixture recovery did not settle before its deadline"
        except BaseException as error:
            failure = error
        # Retain known obligations even when a recovery scan itself fails.
        for _, descriptor in sorted(self.identities):
            if descriptor in preserved:
                continue
            try:
                try:
                    signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                if not self.exited(descriptor, 2000):
                    signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                    assert self.exited(descriptor, 3000), "owned fixture cleanup did not settle"
            except BaseException as error:
                if failure is None:
                    failure = error
            finally:
                os.close(descriptor)
        self.identities = [identity for identity in self.identities if identity[1] in preserved]
        for child, descriptor in self.children:
            try:
                if descriptor in preserved:
                    continue
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        child.kill()
                child.wait(timeout=5)
            except BaseException as error:
                if failure is None:
                    failure = error
        if failure is not None:
            raise failure


def write_atomic(path, value):
    partial = path.with_suffix(".partial")
    partial.write_text(json.dumps(value))
    partial.replace(path)


def create_source(directory):
    root = directory / "steam-fixture"
    common = root / "steamapps/common"
    common.mkdir(parents=True)
    working = directory / "outside-install-tree"
    working.mkdir(mode=0o700)
    home = directory / "isolated-home"
    home.mkdir(mode=0o700)
    data = directory / "isolated-data"
    data.mkdir(mode=0o700)
    markers = directory / "markers"
    markers.mkdir(mode=0o700)
    covers = root / "appcache/librarycache"
    covers.mkdir(parents=True)
    games = {}
    interpreter = Path(sys.executable).resolve()
    for app_id in [42, 43, 44]:
        install = common / f"Game{app_id}"
        install.mkdir(mode=0o700)
        executable = install / "game-probe"
        shutil.copy2(interpreter, executable)
        executable.chmod(0o700)
        games[str(app_id)] = {"executable":str(executable), "seconds":4 if app_id == 43 else 60}
        (root / f"steamapps/appmanifest_{app_id}.acf").write_text(
            f'"AppState" {{ "appid" "{app_id}" "name" "Controlled Steam {app_id}" '
            f'"installdir" "Game{app_id}" "StateFlags" "4" }}')
        (covers / f"{app_id}_library_600x900.png").write_bytes(solid_portrait_png())
    configuration = directory / "game-map.json"
    write_atomic(configuration, games)
    game_script = directory / "game.py"
    descendant = working / "descendant-probe"
    shutil.copy2(Path("/usr/bin/sleep").resolve(), descendant)
    descendant.chmod(0o700)
    game_script.write_text(f"""import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

stopping = False

def stop(_signal, _frame):
    global stopping
    stopping = True
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
child = subprocess.Popen([{str(descendant)!r}, "60"], cwd={str(working)!r})
try:
    marker = Path({str(markers)!r}) / (sys.argv[1] + "-game.json")
    partial = marker.with_suffix(".partial")
    partial.write_text(json.dumps({{"pid":os.getpid(), "descendant":child.pid}}))
    partial.replace(marker)
    time.sleep(float(sys.argv[2]))
finally:
    if stopping:
        # Stop must signal the outside-install-tree descendant independently.
        # Waiting exceeds the tracker's escalation interval and never signals
        # it, so game self-cleanup cannot hide missing descendant association.
        child.wait(timeout=8)
    else:
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)
""")
    client_script = directory / "client.py"
    client_script.write_text(f"""import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

def stop(_signal, _frame):
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
child = None
try:
    app_id = sys.argv[2] if sys.argv[1] == "-applaunch" else "idle"
    if app_id != "idle":
        game = json.loads(Path({str(configuration)!r}).read_text())[app_id]
        child = subprocess.Popen([game["executable"], {str(game_script)!r}, app_id, str(game["seconds"])], cwd={str(working)!r})
    marker = Path({str(markers)!r}) / (app_id + "-client.json")
    partial = marker.with_suffix(".partial")
    partial.write_text(json.dumps({{"steam_pid":os.getppid(), "helper_pid":os.getpid(), "arguments":sys.argv[1:]}}))
    partial.replace(marker)
    until = time.monotonic() + 90
    while time.monotonic() < until:
        if child is not None:
            child.poll()
        time.sleep(0.05)
finally:
    if child is not None:
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=3)
""")
    # Native fixture keeps an actual /proc executable named steam while the
    # helper owns/reaps its game. It preserves the imported literal argv boundary.
    client_source = directory / "steam.c"
    client_source.write_text(f"""#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {{
    if (!((argc == 2 && strcmp(argv[1], "--idle") == 0) ||
          (argc == 3 && strcmp(argv[1], "-applaunch") == 0))) return 2;
    if (chdir({json.dumps(str(working))}) != 0) return 8;
    pid_t parent = getpid();
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) {{
        /* No helper survives an early fixture-wrapper failure. No children
           exist until exec, and the Python helper owns/reaps later game children. */
        if (prctl(PR_SET_PDEATHSIG, SIGTERM) != 0 || getppid() != parent) _exit(7);
        execl({json.dumps(str(interpreter))}, {json.dumps(str(interpreter))},
              {json.dumps(str(client_script))}, argv[1], argc == 3 ? argv[2] : NULL, (char *)NULL);
        _exit(4);
    }}
    int status = 0;
    while (waitpid(child, &status, 0) < 0) {{ if (errno != EINTR) return 5; }}
    return WIFEXITED(status) ? WEXITSTATUS(status) : 6;
}}
""")
    steam = root / "steam"
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", client_source, "-o", steam], check=True, timeout=30)
    steam.chmod(0o700)
    environment = os.environ.copy()
    environment.update(HOME=str(home), XDG_DATA_HOME=str(data), PATH=str(root))
    return root, steam.resolve(), interpreter, game_script, client_script, configuration, games, markers, working, environment


def marker(path):
    deadline = time.monotonic() + 5
    while not path.exists() and time.monotonic() < deadline:
        time.sleep(0.025)
    assert path.exists(), f"fixture marker absent: {path.name}"
    return json.loads(path.read_text())


def imported(api, bearer, steam, games):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        status = post(api, "/v1/steam/status", {}, bearer)
        page = post(api, "/v1/catalog/list", {"limit":32}, bearer)
        rows = {row["entry"].get("steam_id"):row for row in page["entries"]}
        if (all(app_id in rows for app_id in games) and not status["catalog_sync"]["syncing"]
                and not status["scanning"] and not status["force_sync_pending"]):
            assert status["complete"] and status["warning_count"] == 0, status
            assert status["catalog_sync"]["last_error"] is None, status
            for app_id in games:
                entry = rows[app_id]["entry"]
                assert entry["steam_install_dir"] == str(Path(games[app_id]["executable"]).parent), entry
                assert entry["launch"]["program"] == str(steam), entry
                assert entry["launch"]["arguments"] == ["-applaunch", app_id], entry
            return rows
        time.sleep(0.04)
    raise AssertionError(f"Steam fixture import failed: {status}, {page}")


def ready_settings(console, api, bearer):
    current = None
    for _ in range(100):
        if console.request("amaterasu.management/1", {"action":"product_get"}).get("ok"):
            current = post(api, "/v1/steam/settings", {}, bearer)
            if current.get("ok"):
                return current
        time.sleep(0.025)
    raise AssertionError(f"Steam settings did not load: {current}")


def allow_fixture(cohort, steam, interpreter, game_script, client_script, games, working):
    cohort.allow(steam, [steam, "--idle"], working, 3)
    cohort.allow(interpreter, [interpreter, client_script, "--idle"], working, 2)
    descendant = working / "descendant-probe"
    cohort.allow(descendant, [descendant, "60"], working, 0)
    for app_id, game in games.items():
        cohort.allow(steam, [steam, "-applaunch", app_id], working, 3)
        # Launch infers the main executable directory before the native wrapper
        # changes cwd. Recover its exact identity in either state, even when an
        # early assertion ends admission before the wrapper reaches chdir/fork.
        cohort.allow(steam, [steam, "-applaunch", app_id], steam.parent, 3)
        cohort.allow(interpreter, [interpreter, client_script, "-applaunch", app_id], working, 2)
        executable = Path(game["executable"])
        cohort.allow(executable, [executable, game_script, app_id, str(game["seconds"])], working, 1)


class PreMarkerAbort(RuntimeError):
    pass


def begin_game(api, bearer, row, operation, cohort, steam, interpreter, game_script, client_script,
               game, markers, abort_before_marker=False):
    app_id = row["entry"]["steam_id"]
    for suffix in ["client", "game"]:
        (markers / f"{app_id}-{suffix}.json").unlink(missing_ok=True)
    request = {"operation_id":operation, "app_id":row["id"]}
    started = post(api, "/v1/launch/start", request, bearer)["launch"]
    active = wait_state(api, started["launch_id"], bearer, "active")
    if abort_before_marker:
        raise PreMarkerAbort("injected failure before launcher/child ownership markers")
    client = marker(markers / f"{app_id}-client.json")
    assert client["arguments"] == ["-applaunch", app_id], client
    helper = cohort.remember(client["helper_pid"], interpreter,
                             [interpreter, client_script, "-applaunch", app_id], 2, client["steam_pid"])
    launcher = cohort.remember(client["steam_pid"], steam, [steam, "-applaunch", app_id], 3)
    observed = marker(markers / f"{app_id}-game.json")
    executable = Path(game["executable"])
    sleep = markers.parent / "outside-install-tree/descendant-probe"
    descendant = cohort.remember(observed["descendant"], sleep, [sleep, "60"], 0, observed["pid"])
    game_identity = cohort.remember(observed["pid"], executable,
                                    [executable, game_script, app_id, str(game["seconds"])], 1, client["helper_pid"])
    assert Path(f'/proc/{observed["pid"]}/cwd').resolve() == markers.parent / "outside-install-tree"
    assert active["executable_path"] == str(steam), active
    retry = post(api, "/v1/launch/start", request, bearer)["launch"]
    assert retry["launch_id"] == active["launch_id"] and retry["process_id"] == active["process_id"], retry
    return active, [game_identity, descendant], [launcher, helper]


def main():
    if not sys.platform.startswith("linux") or os.geteuid() == 0:
        raise RuntimeError("this acceptance requires non-root Linux, pidfds and a C compiler")
    if "--skip-build" not in sys.argv[1:]:
        run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-steam-launch-") as temporary:
        directory = Path(temporary)
        root, steam, interpreter, game_script, client_script, configuration, games, markers, working, environment = create_source(directory)
        cohort = OwnedProcesses()
        allow_fixture(cohort, steam, interpreter, game_script, client_script, games, working)
        console = None
        try:
            original = cohort.start([steam, "--idle"], steam, environment, working, 3)
            idle = marker(markers / "idle-client.json")
            idle_helper = cohort.remember(idle["helper_pid"], interpreter, [interpreter, client_script, "--idle"], 2, idle["steam_pid"])
            baseline_program = Path(games["42"]["executable"])
            baseline_args = [baseline_program, "-c", "import time; time.sleep(90)"]
            baseline = cohort.start(baseline_args, baseline_program, environment, working)
            harness = PlatformHarness(directory)
            bootstrap_path = harness.root / "bootstrap.json"
            bootstrap = json.loads(bootstrap_path.read_text())
            bootstrap["policy"]["settings"]["amaterasu.management"].update(enabled=True, bind="127.0.0.1:0")
            bootstrap["policy"]["settings"]["amaterasu.steam"] = {"roots":[str(root)]}
            bootstrap["policy"]["settings"]["amaterasu.nvhttp"]["enabled"] = False
            bootstrap_path.write_text(json.dumps(bootstrap))
            harness.stage(harness.package(1))
            console = Console(harness, environment=environment)
            assert console.request("amaterasu.authentication/1", {"action":"setup", "username":"steam-test", "password":"isolated-steam-test-password"})["ok"]
            bearer = token(console, ["configuration.read", "configuration.write", "catalog.read", "steam.read", "steam.refresh", "application.read", "application.launch", "application.terminate"])
            api = Api(console)
            current = ready_settings(console, api, bearer)
            changed = post(api, "/v1/steam/settings_update", {"enabled":True, "auto_sync":False,
                "exclude_games":current["values"]["exclude_games"], "sync_all_installed":True,
                "include_tools":True, "expected_revision":current["revision"]}, bearer)
            assert changed["values"]["sync_all_installed"] is True and changed["values"]["include_tools"] is True, changed
            post(api, "/v1/steam/force_sync", {}, bearer)
            rows = imported(api, bearer, steam, games)
            survivors = [original, idle_helper, baseline]
            try:
                begin_game(api, bearer, rows["42"], "pre-marker-abort", cohort,
                    steam, interpreter, game_script, client_script, games["42"], markers,
                    abort_before_marker=True)
            except PreMarkerAbort:
                # End admission before recovering a launch which never reached
                # marker ownership registration. Detached Steam clients survive
                # host teardown, so cleanup must discover their exact identities.
                console.eof()
                console.close()
                console = None
                registered = len(cohort.identities)
                assert cohort.recover() >= 2, "pre-marker abort did not recover detached fixtures"
                recovered = cohort.identities[registered:]
                assert any(priority == 3 for priority, _ in recovered), "native launcher was not recovered"
                cohort.cleanup(preserve=survivors)
                cohort.assert_alive(survivors)
            else:
                raise AssertionError("pre-marker failure injection did not fire")
            console = Console(harness, environment=environment)
            api = Api(console)
            ready_settings(console, api, bearer)
            post(api, "/v1/steam/force_sync", {}, bearer)
            rows = imported(api, bearer, steam, games)
            for operation in ["owned-stop", "lease-reacquire"]:
                active, tracked, launchers = begin_game(api, bearer, rows["42"], operation, cohort,
                    steam, interpreter, game_script, client_script, games["42"], markers)
                # Created after the tracker baseline, with game-path text in argv
                # but neither install-tree executable nor cwd evidence.
                unrelated_args = [interpreter, "-c", "import time; time.sleep(90)", games["42"]["executable"]]
                unrelated = cohort.start(unrelated_args, interpreter, environment, working)
                survivors.extend([unrelated, *launchers])
                post(api, "/v1/launch/stop", {"launch_id":active["launch_id"]}, bearer)
                stopped = wait_state(api, active["launch_id"], bearer, "stopped")
                assert stopped["cleanup_error"] is None, stopped
                assert all(cohort.exited(descriptor, 3000) for descriptor in tracked), "attributed game tree survived stop"
                cohort.assert_alive(survivors)
            active, tracked, launchers = begin_game(api, bearer, rows["43"], "natural-exit", cohort,
                steam, interpreter, game_script, client_script, games["43"], markers)
            survivors.extend(launchers)
            natural = wait_state(api, active["launch_id"], bearer, "stopped")
            assert natural["cleanup_error"] is None and natural["failure"] is None, natural
            assert all(cohort.exited(descriptor, 3000) for descriptor in tracked), "natural game tree did not exit"
            cohort.assert_alive(survivors)

            # Preserve the imported path, but make its canonical directory absent.
            # The controlled client still launches a known game at its moved path.
            old_install = Path(games["44"]["executable"]).parent
            moved_install = old_install.with_name("MovedGame44")
            old_install.rename(moved_install)
            games["44"]["executable"] = str(moved_install / "game-probe")
            write_atomic(configuration, games)
            allow_fixture(cohort, steam, interpreter, game_script, client_script, games, working)
            active, untracked, launchers = begin_game(api, bearer, rows["44"], "unknown-install", cohort,
                steam, interpreter, game_script, client_script, games["44"], markers)
            survivors.extend(launchers)
            post(api, "/v1/launch/stop", {"launch_id":active["launch_id"]}, bearer)
            stopped = wait_state(api, active["launch_id"], bearer, "stopped")
            assert stopped["cleanup_error"] is None, stopped
            cohort.assert_alive([*survivors, *untracked])
        finally:
            try:
                if console is not None:
                    try:
                        stop_console(console)
                    finally:
                        console.close()
            finally:
                cohort.cleanup()
    print("Passed: signed Linux Steam import and literal -applaunch metadata; real install-tree game/descendant association, owned stop and lease reacquisition, natural exit, baseline/client/unrelated survival, absent-install no-signaling, pre-marker abort recovery and pidfd cleanup. Controlled executables; real Steam/Proton/game delivery remains unqualified.")


if __name__ == "__main__":
    main()
