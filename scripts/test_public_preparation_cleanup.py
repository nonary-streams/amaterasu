#!/usr/bin/env python3
"""Signed public preparation/output and cleanup lasting beyond twenty seconds.

Uses private controlled Python children, synthetic capture/input, paired HTTPS,
encrypted RTSP and the independent ENet client. This qualifies protocol and
lifecycle behavior on Linux, not hardware capture, playback or installed hosts.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import secrets
import shlex
import signal
import socket
import sys
import tempfile
import time
from urllib.parse import urlsplit


def process_start(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    except (FileNotFoundError, ProcessLookupError):
        return None


def controlled_child(arguments):
    trace, event, delay = arguments
    identity = {"pid": os.getpid(), "start": process_start(os.getpid())}

    def record(phase):
        row = {**identity, "event": event, "phase": phase, "at": time.monotonic()}
        with Path(trace).open("a") as output:
            output.write(json.dumps(row) + "\n")

    record("start")
    print("OUT-" + event, flush=True)
    print("ERR-" + event, file=sys.stderr, flush=True)
    time.sleep(float(delay))
    record("done")


# Configured commands use a cleared environment and only need the standard
# library. Dispatch them before importing the integration client's dependencies.
if __name__ == "__main__" and len(sys.argv) > 1 and sys.argv[1] == "--child":
    controlled_child(sys.argv[2:])
    raise SystemExit(0)

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_management_api import Api, PASSWORD, stop_console
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_public_launch import (
    SOURCE, capture_idle, launch_query, media_complete, wait_until,
)
from test_public_launch_workflow import configure
from test_rtsp import PeerDiscovery, RtspClient, announcement
from test_software_encoder_policy import checked, login
from test_steam_launch_workflow import OwnedProcesses
from test_stream_session import start_client, stop_client

LONG_UNDO_SECONDS = 21
INPUT_PROBE = "test.input-probe/1"
OUTPUT_ROOT = "AMATERASU_OUTPUT_FIXTURE_ROOT"
SCRIPT = Path(__file__).resolve()
PYTHON = Path(sys.executable).resolve()


def trace_rows(path):
    if not path.exists():
        return []
    rows = []
    lines = path.read_text().splitlines()
    for index, line in enumerate(lines):
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            assert index == len(lines) - 1, "malformed completed trace record"
    return rows


def child_arguments(trace, event, delay):
    return [str(PYTHON), str(SCRIPT), "--child", str(trace), event, str(delay)]


def structured_command(owned, directory, trace, event, delay):
    arguments = child_arguments(trace, event, delay)
    owned.allow(PYTHON, arguments, directory, 1)
    return {
        "executable": arguments[0], "args": arguments[1:],
        "cwd": str(directory), "timeout_ms": 30_000,
    }


def raw_command(owned, directory, trace, event, delay):
    arguments = child_arguments(trace, event, delay)
    owned.allow(PYTHON, arguments, directory, 1)
    return shlex.join(arguments)


def create_application(api, bearer, directory, owned, label, lifetime, *, prepared=True,
                       failed_output=False):
    trace = directory / (label + ".trace")
    output = directory / (label + ".log")
    arguments = child_arguments(trace, "main" if prepared else "probe", lifetime)
    owned.allow(PYTHON, arguments, directory, 1)
    configured_output = f"$({OUTPUT_ROOT})/{label}.log"
    if failed_output:
        configured_output = f"$({OUTPUT_ROOT})/missing-parent/output.log"
    else:
        output.write_text("existing-" + label + "\n")
    entry = {
        "name": "Preparation cleanup " + label,
        "output": configured_output,
        "config_overrides": {
            "frame_limiter_enable": False,
            "frame_limiter_fps_limit": 0,
        },
        "launch": {
            "kind": "executable", "program": arguments[0],
            "arguments": arguments[1:], "working_directory": str(directory),
            "auto_detach": False, "wait_all": True, "exit_timeout_seconds": 30,
        },
    }
    if prepared:
        entry["launch"]["preparation"] = [{
            "run": structured_command(owned, directory, trace, "structured-do", 0.05),
            "undo": structured_command(
                owned, directory, trace, "structured-undo", LONG_UNDO_SECONDS,
            ),
        }]
        entry["prep_cmd"] = [{
            "do": raw_command(owned, directory, trace, "raw-do", 0.05),
            "undo": raw_command(owned, directory, trace, "raw-undo", 0.05),
        }]
    for _ in range(5):
        catalog = checked(api, "/v1/catalog/list", bearer=bearer)
        status, created, _ = api.post("/v1/catalog/create", {
            "expected_revision": catalog["revision"], "entry": entry,
        }, bearer)
        assert status == 200, (status, created)
        if created.get("code") != "revision_conflict":
            break
    assert created.get("ok"), created
    saved = checked(api, "/v1/catalog/get", {"id": created["id"]}, bearer)
    assert saved["entry"]["output"] == configured_output, saved
    assert saved["entry"]["config_overrides"]["frame_limiter_enable"] is False, saved
    assert saved["entry"]["config_overrides"]["frame_limiter_fps_limit"] == 0, saved
    if prepared:
        for key in ("do", "undo"):
            assert saved["entry"]["prep_cmd"][0][key] == entry["prep_cmd"][0][key], saved
        assert saved["entry"]["launch"]["preparation"][0]["undo"]["timeout_ms"] == 30_000
    return {"id": created["id"], "entry": saved["entry"], "trace": trace,
            "output": output, "label": label}


def initialize(console):
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
        "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
    })
    assert imported["ok"], imported
    wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
             lambda value: value.get("tls_ready"))
    assert console.request("amaterasu.authentication/1", {
        "action": "setup", "username": "administrator", "password": PASSWORD,
    })["ok"]
    api = Api(console)
    bearer = login(api)
    settings = checked(api, "/v1/nvhttp/settings", bearer=bearer)
    encrypted = checked(api, "/v1/nvhttp/settings_update", {
        "hostname": settings["configured_hostname"], "port": settings["port"],
        "expected_revision": settings["revision"], "lan_encryption_mode": 2,
    }, bearer)
    assert encrypted["lan_encryption_mode"] == 2, encrypted
    return api, bearer


def public_idle(https, context):
    state = successful(get(https, "/serverinfo", {"uniqueid": "preparation-cleanup"}, context))
    return state.findtext("state") == "AMATERASU_SERVER_FREE" and state.findtext("currentgame") == "0"


def original_children_gone(trace):
    return all(process_start(row["pid"]) != row["start"]
               for row in trace_rows(trace) if row["phase"] == "start")


def wait_for_settlement(console, https, context, app, owned, baseline, *, opened_delta=0):
    def settled():
        owned.recover()
        complete = any(row["event"] == "structured-undo" and row["phase"] == "done"
                       for row in trace_rows(app["trace"]))
        return complete and original_children_gone(app["trace"]) and public_idle(https, context)

    wait_until(settled, "public cleanup did not settle automatically after slow undo", 45)
    assert capture_idle(console, baseline, opened_delta=opened_delta), "capture lease retained"
    assert not console.request(INPUT_PROBE, {"action": "status"})["active"], "input lease retained"


def assert_lifecycle(app, *, natural):
    rows = trace_rows(app["trace"])
    starts = [row for row in rows if row["phase"] == "start"]
    assert [row["event"] for row in starts] == [
        "structured-do", "raw-do", "main", "raw-undo", "structured-undo",
    ], rows
    completed = [row["event"] for row in rows if row["phase"] == "done"]
    assert completed == ["structured-do", "raw-do", *(["main"] if natural else []),
                         "raw-undo", "structured-undo"], rows
    slow_start = next(row for row in starts if row["event"] == "structured-undo")
    slow_done = next(row for row in rows if row["event"] == "structured-undo" and row["phase"] == "done")
    elapsed = slow_done["at"] - slow_start["at"]
    assert elapsed >= LONG_UNDO_SECONDS, elapsed
    output = app["output"].read_text()
    assert output.startswith("existing-" + app["label"] + "\n"), output
    for row in starts:
        for stream in ("OUT", "ERR"):
            assert output.splitlines().count(stream + "-" + row["event"]) == 1, output
    assert original_children_gone(app["trace"]), rows
    return {"undo_seconds": elapsed, "events": [row["event"] + ":" + row["phase"] for row in rows],
            "output": output.splitlines()}


def probe_reacquisition(console, https, context, app, owned, exported):
    previous = len(trace_rows(app["trace"]))
    opened = successful(get(https, "/launch", launch_query(secrets.token_bytes(16), exported), context))
    assert opened.findtext("sessionUrl0", "").startswith("rtspenc://"), opened.attrib

    def completed():
        owned.recover()
        return (len(trace_rows(app["trace"])) == previous + 2
                and original_children_gone(app["trace"]) and public_idle(https, context))

    wait_until(completed, "new public launch failed to reacquire and settle its leases", 10)


def failed_open_has_no_effects(console, https, context, app, owned, exported):
    baseline = console.request(SOURCE, {"action": "status"})
    status, rejected = get(https, "/launch", launch_query(secrets.token_bytes(16), exported), context)
    assert status != 200 or rejected.get("status_code") != "200", (status, rejected.attrib)
    wait_until(lambda: public_idle(https, context), "failed output launch retained public ownership", 10)
    owned.recover()
    assert not app["trace"].exists(), "failed output open ran a command"
    assert not (app["output"].parent / "missing-parent" / "output.log").exists()
    assert capture_idle(console, baseline), "failed output open changed capture state"
    print("Passed: failed output open produced no command/capture effects", flush=True)


def teardown_stream(console, oracle, directory, url, key, baseline):
    target = urlsplit(url)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        discovery = PeerDiscovery(video, audio)
        client = RtspClient(f"{target.hostname}:{target.port}", url, key)
        control = None
        try:
            ports = client.setup(discovery.on_setup)
            client.transact("ANNOUNCE", "streamid=control/13/0",
                            {"Content-Type": "application/sdp"}, announcement())
            client.transact("PLAY", "/")
            control = start_client(oracle, directory, {
                "control_local": f"127.0.0.1:{ports['control']}",
            }, key, ports["connect_data"])
            discovery.finish(console, baseline)
            wait_until(lambda: media_complete(console, baseline), "synthetic stream did not start", 5)
            wait_until(lambda: console.request(INPUT_PROBE, {"action": "status"})["active"],
                       "synthetic input lease did not start")
            client.transact("TEARDOWN", "/")
        finally:
            if control is not None:
                stop_client(control)
            if "video" in discovery.destinations:
                discovery.close()
            else:
                discovery.decoy.close()


def exercise_case(console, https, context, app, owned, exported, mode, oracle, directory):
    baseline = console.request(SOURCE, {"action": "status"})
    key = secrets.token_bytes(16)
    query = launch_query(key, exported)
    opened = successful(get(https, "/launch", query, context))
    url = opened.findtext("sessionUrl0")
    assert url and url.startswith("rtspenc://"), url
    wait_until(lambda: any(row["event"] == "main" and row["phase"] == "start"
                           for row in trace_rows(app["trace"])), "application did not start")
    owned.recover()
    assert successful(get(https, "/launch", query, context)).findtext("sessionUrl0") == url
    cancel_timed_out = False
    started = time.monotonic()
    if mode == "cancel":
        try:
            successful(get(https, "/cancel", context=context))
        except (TimeoutError, socket.timeout):
            cancel_timed_out = True
        assert cancel_timed_out, "slow cleanup did not exercise the disconnected HTTPS cancel client"
        print("HTTPS cancel client timed out; observing retained background cleanup", flush=True)
    elif mode == "teardown":
        teardown_stream(console, oracle, directory, url, key, baseline)
    else:
        assert mode == "natural"
    wait_for_settlement(console, https, context, app, owned, baseline,
                        opened_delta=int(mode == "teardown"))
    evidence = assert_lifecycle(app, natural=mode == "natural")
    evidence.update(mode=mode, cleanup_observation_seconds=time.monotonic() - started,
                    https_cancel_timed_out=cancel_timed_out)
    print(json.dumps(evidence, sort_keys=True), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true", help="use already-built signed fixture artifacts")
    arguments = parser.parse_args()
    assert sys.platform == "linux" and hasattr(os, "pidfd_open") and hasattr(signal, "pidfd_send_signal"), \
        "this controlled-process qualification requires Linux pidfd ownership"
    oracle = Path(os.environ["AMATERASU_ENET_CLIENT_ORACLE"]).resolve(strict=True)
    if not arguments.skip_build:
        run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-public-preparation-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        configure(harness, synthetic=True)
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        settings = bootstrap["policy"]["settings"]
        settings["amaterasu.launch"] = {"environment": {OUTPUT_ROOT: str(directory)}}
        settings["amaterasu.stream-session"]["enable_input"] = True
        settings["amaterasu.input-probe"] = {"enabled": True}
        path.write_text(json.dumps(bootstrap))
        package = directory / "signed-stream-probe"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        run(harness.publisher, "verify", harness.root, package)
        harness.stage(package)
        owned = OwnedProcesses()
        console = Console(harness)
        try:
            api, bearer = initialize(console)
            applications = {
                mode: create_application(api, bearer, directory, owned, mode,
                                         3 if mode == "natural" else 120)
                for mode in ("natural", "cancel", "teardown")
            }
            applications["probe"] = create_application(api, bearer, directory, owned,
                                                        "probe", 1, prepared=False)
            applications["failed"] = create_application(api, bearer, directory, owned,
                                                         "failed", 0, failed_output=True)
            console.eof()
            console = Console(harness)
            state = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
                             lambda value: value.get("tls_ready"))
            api = Api(console)
            bearer = login(api)
            for app in applications.values():
                readback = checked(api, "/v1/catalog/get", {"id": app["id"]}, bearer)
                assert readback["entry"] == app["entry"], readback
            print("Passed: signed catalog output/raw preparation readback survives host restart", flush=True)
            pair(console, "http://" + state["http_address"])
            https, context = "https://" + state["https_address"], tls_context()
            library = successful(get(https, "/applist", context=context))
            exported = {}
            for mode, app in applications.items():
                matches = [row.findtext("ID") for row in library.findall("App")
                           if row.findtext("AppTitle") == app["entry"]["name"]]
                assert len(matches) == 1 and matches[0].isdecimal(), matches
                exported[mode] = matches[0]
            failed_open_has_no_effects(console, https, context, applications["failed"], owned, exported["failed"])
            probe_reacquisition(console, https, context, applications["probe"], owned, exported["probe"])
            control_directory = directory / "control"
            control_directory.mkdir()
            for mode in ("natural", "cancel", "teardown"):
                print("Starting >20-second public cleanup case: " + mode, flush=True)
                exercise_case(console, https, context, applications[mode], owned, exported[mode],
                              mode, oracle, control_directory)
                probe_reacquisition(console, https, context, applications["probe"], owned, exported["probe"])
                print("Passed: fresh public launch reacquired leases after " + mode, flush=True)
            console.eof()
        except BaseException:
            print("".join(console.errors), file=sys.stderr)
            raise
        finally:
            try:
                stop_console(console)
            finally:
                owned.cleanup()
        assert all(original_children_gone(app["trace"]) for app in applications.values())
    print("Passed: signed paired HTTPS natural exit/cancel and encrypted RTSP TEARDOWN each "
          "settle >20-second preparation cleanup automatically, append actual stdout/stderr, "
          "run reverse undo once and reacquire leases. Synthetic capture/input and controlled "
          "Linux children; hardware, playback and other platforms remain unqualified.", flush=True)


if __name__ == "__main__":
    main()
