#!/usr/bin/env python3
"""Real Chromium against signed native HTTPS management and production assets.

All sockets are ephemeral loopback. TLS identities, host and browser HOME, XDG
state and browser profiles are temporary; no installed CA, browser preference or service is changed.
Chromium's certificate exception is restricted to one newly generated SPKI key.
"""
from __future__ import annotations

import argparse
import base64
from datetime import datetime, timedelta, timezone
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import ipaddress
import json
import os
from pathlib import Path
import queue
import secrets
import select
import shutil
import signal
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import threading
import time
import zipfile

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import solid_portrait_png, stop_console
from test_management_origin_policy import SERVICE, ready
from test_platform_modules import PlatformHarness, configure_steam_fixture


def identity(directory):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    directory.mkdir(mode=0o700)
    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Private signed-browser test")])
    now = datetime.now(timezone.utc)
    certificate = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
        .public_key(key.public_key()).serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=1)).not_valid_after(now + timedelta(hours=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(x509.SubjectAlternativeName([
            x509.IPAddress(ipaddress.ip_address("127.0.0.1")),
        ]), critical=False).sign(key, hashes.SHA256()))
    material = {
        "cert": certificate.public_bytes(serialization.Encoding.PEM),
        "key": key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                 serialization.NoEncryption()),
    }
    files = {}
    for role, contents in material.items():
        path = directory / f"management-{role}-{hashlib.sha256(contents).hexdigest()}.pem"
        path.write_bytes(contents)
        path.chmod(0o600)
        files[role] = path
    public = key.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
    return files, base64.b64encode(hashlib.sha256(public).digest()).decode("ascii")


def configure_host(harness):
    configure_steam_fixture(harness)
    steam_root = harness.directory / "steam-library"
    steam = steam_root / "steam"
    steam.write_text("#!/bin/sh\nexit 0\n")
    steam.chmod(0o700)
    artwork = steam_root / "appcache/librarycache"
    artwork.mkdir(parents=True)
    (artwork / "730_library_600x900.png").write_bytes(solid_portrait_png())
    state = harness.root / "data/state"
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    files, spki = identity(state / "management-tls")
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.management"].update({
        "enabled": True, "bind": "127.0.0.1:0", "tls_bind": f"127.0.0.1:{port}",
        "tls_certificate_path": "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + files["cert"].name,
        "tls_private_key_path": "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + files["key"].name,
    })
    bootstrap["policy"]["settings"]["amaterasu.nvhttp"] = {
        "enabled": True, "http_bind": "127.0.0.1:0", "https_bind": "127.0.0.1:0",
    }
    # The isolated browser fixture seeds its own pairing identity; management
    # HTTPS still uses the independent loopback certificate above.
    bootstrap["policy"]["settings"]["amaterasu.pairing"]["host_configuration"] = False
    path.write_text(json.dumps(bootstrap))
    return files["cert"], spki, f"https://127.0.0.1:{port}"


class RejectKeyProbe:
    def __init__(self, directory):
        files, _ = identity(directory)

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"unrelated test TLS identity")

            def log_message(self, _format, *_arguments):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(files["cert"], files["key"])
        self.server.socket = context.wrap_socket(self.server.socket, server_side=True)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.origin = f"https://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)
        assert not self.thread.is_alive(), "negative TLS listener did not stop"


class OwnedBrowser:
    """Retain pidfds for only this browser generation's private child lineage.

    Direct Popen ownership seeds the retained root pidfd. Verified kernel parent
    relationships recover helpers that sanitize their environment. A private
    inherited marker also identifies breakaway crash handlers. Recovery rechecks
    same-user ownership, start time, executable and parent/marker evidence under
    a pidfd before retaining each handle.
    No numeric PID or process-name signaling is used for recovered helpers.
    """

    marker_name = b"AMATERASU_SIGNED_BROWSER_FIXTURE"

    def __init__(self, directory, executable, spki, reuse_profile=False, extra_args=()):
        self.directory = directory
        self.directory.mkdir(mode=0o700, exist_ok=reuse_profile)
        self.profile = directory / "profile"
        self.profile.mkdir(mode=0o700, exist_ok=reuse_profile)
        self.executable = executable
        self.spki = spki
        self.extra_args = tuple(extra_args)
        self.child = None
        self.descriptors = {}
        self.marker = secrets.token_hex(32).encode("ascii")
        self.errors = (directory / f"browser-{time.time_ns()}.stderr").open("w")
        Path(self.errors.name).chmod(0o600)

    def belongs(self, process):
        if process.stat().st_uid != os.geteuid():
            return False
        entries = (process / "environ").read_bytes().split(b"\0")
        return self.marker_name + b"=" + self.marker in entries

    def recover(self):
        entries = list(Path("/proc").iterdir())
        assert len(entries) <= 100000, "browser ownership enumeration exceeded its bound"
        recovered = 0
        for _ in range(32):
            added = False
            for process in entries:
                if not process.name.isdecimal():
                    continue
                descriptor = None
                try:
                    if process.stat().st_uid != os.geteuid():
                        continue
                    initial = (process / "stat").read_text().rsplit(")", 1)[1].split()
                    before, parent = initial[19], int(initial[1])
                    key = (int(process.name), before)
                    if key in self.descriptors:
                        continue
                    parent_keys = [known for known, fd in self.descriptors.items()
                                   if known[0] == parent and not self.exited(fd)]
                    try:
                        inherited = self.belongs(process)
                    except PermissionError:
                        inherited = False
                    if not inherited and not parent_keys:
                        continue
                    executable = (process / "exe").resolve(strict=True)
                    descriptor = os.pidfd_open(key[0])
                    current = (process / "stat").read_text().rsplit(")", 1)[1].split()
                    if (process.stat().st_uid != os.geteuid() or current[19] != before
                            or (process / "exe").resolve(strict=True) != executable):
                        continue
                    if inherited:
                        if not self.belongs(process):
                            continue
                    else:
                        if int(current[1]) != parent:
                            continue
                        parent_stat = Path(f"/proc/{parent}/stat").read_text().rsplit(")", 1)[1].split()
                        if not any(known[1] == parent_stat[19] for known in parent_keys):
                            continue
                    self.descriptors[key] = descriptor
                    descriptor = None
                    recovered += 1
                    added = True
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    pass
                finally:
                    if descriptor is not None:
                        os.close(descriptor)
            if not added:
                return recovered
        raise AssertionError("browser helper ancestry exceeded its recovery bound")

    @staticmethod
    def exited(descriptor, timeout=0):
        polling = select.poll()
        polling.register(descriptor, select.POLLIN)
        events = polling.poll(timeout)
        assert all(not event & (select.POLLERR | select.POLLNVAL) for _, event in events), "browser pidfd became invalid"
        return any(event & select.POLLIN for _, event in events)

    def start(self):
        active = self.profile / "DevToolsActivePort"
        active.unlink(missing_ok=True)
        environment = os.environ.copy()
        environment.update(HOME=str(self.directory), XDG_CONFIG_HOME=str(self.directory / "config"),
                           XDG_CACHE_HOME=str(self.directory / "cache"), XDG_DATA_HOME=str(self.directory / "data"))
        environment[self.marker_name.decode()] = self.marker.decode()
        self.child = subprocess.Popen([
            self.executable, "--headless=new", "--enable-automation", "--disable-gpu", "--no-first-run",
            "--no-default-browser-check", "--disable-extensions", "--disable-background-networking",
            "--disable-sync", "--password-store=basic", "--disable-breakpad",
            "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0",
            "--ignore-certificate-errors-spki-list=" + self.spki,
            "--user-data-dir=" + str(self.profile), *self.extra_args, "about:blank",
        ], env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=self.errors, start_new_session=True)
        root = Path(f"/proc/{self.child.pid}")
        initial = (root / "stat").read_text().rsplit(")", 1)[1].split()
        descriptor = os.pidfd_open(self.child.pid)
        try:
            assert root.stat().st_uid == os.geteuid() and int(initial[1]) == os.getpid(), "direct browser child identity changed"
            assert (root / "stat").read_text().rsplit(")", 1)[1].split()[19] == initial[19], "direct browser start time changed"
            self.descriptors[(self.child.pid, initial[19])] = descriptor
        except BaseException:
            os.close(descriptor)
            raise
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            self.recover()
            if active.exists():
                lines = active.read_text().splitlines()
                assert len(lines) == 2 and lines[0].isdecimal(), "unexpected browser debugger record"
                assert 0 < int(lines[0]) <= 65535 and lines[1].startswith("/devtools/browser/"), "unexpected debugger endpoint"
                self.recover()
                assert any(pid == self.child.pid for pid, _ in self.descriptors), "direct browser pidfd was not retained"
                return f"ws://127.0.0.1:{lines[0]}{lines[1]}"
            time.sleep(0.05)
        raise AssertionError("owned browser did not publish its loopback debugger")

    def close(self):
        failure = None
        retained = len(self.descriptors)
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                self.recover()
                live = [descriptor for descriptor in self.descriptors.values() if not self.exited(descriptor)]
                if not live:
                    time.sleep(0.05)
                    if self.recover() == 0:
                        break
                for descriptor in live:
                    try:
                        signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                time.sleep(0.1)
                for descriptor in live:
                    if not self.exited(descriptor, 500):
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        assert self.exited(descriptor, 2000), "owned browser helper did not exit"
            else:
                raise AssertionError("owned browser cleanup did not settle")
            assert self.recover() == 0 and all(self.exited(fd) for fd in self.descriptors.values()), "browser cleanup left a live owned process"
        except BaseException as error:
            failure = error
        finally:
            # A failed recovery or one failed helper must not abandon any other
            # already-retained obligation. Every known handle receives its own
            # bounded termination and exit confirmation before being closed.
            for descriptor in self.descriptors.values():
                try:
                    if not self.exited(descriptor):
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                    if not self.exited(descriptor, 500):
                        try:
                            signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        assert self.exited(descriptor, 2000), "retained browser obligation did not exit"
                except BaseException as error:
                    if failure is None:
                        failure = error
            if self.child is not None:
                try:
                    # Direct Popen ownership also covers failure before recovery.
                    if self.child.poll() is None:
                        self.child.kill()
                    self.child.wait(timeout=5)
                except BaseException as error:
                    if failure is None:
                        failure = error
            for descriptor in self.descriptors.values():
                os.close(descriptor)
            self.descriptors.clear()
            self.errors.close()
        # This independent argv check also detects helpers that did not retain
        # a readable environment marker. Such a process is reported as a failed
        # cleanup; it is never signaled through an unverified numeric PID.
        remaining = 0
        for process in Path("/proc").iterdir():
            if not process.name.isdecimal():
                continue
            try:
                if process.stat().st_uid != os.geteuid():
                    continue
                fields = (process / "stat").read_text().rsplit(")", 1)[1].split()
                if fields[0] == "Z":
                    continue
                arguments = (process / "cmdline").read_bytes().split(b"\0")[:-1]
                for argument in arguments:
                    value = os.fsdecode(argument).partition("=")[2] if argument.startswith(b"--") else os.fsdecode(argument)
                    if value == str(self.directory) or value.startswith(str(self.directory) + "/"):
                        remaining += 1
                        break
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
        if remaining and failure is None:
            failure = AssertionError("browser cleanup left a private-profile process")
        if failure is not None:
            raise failure
        return {"retained_identities": retained, "private_profile_processes_remaining": remaining}


class BrowserDriver:
    def __init__(self):
        self.child = subprocess.Popen([
            "node", str(WORKSPACE / "frontend/tests/signed-browser-qualification.mjs"),
        ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.replies = queue.Queue()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.child.stdout:
            self.replies.put(json.loads(line))
        self.replies.put(None)

    def phase(self, action, **fields):
        self.child.stdin.write(json.dumps({"action": action, **fields}) + "\n")
        self.child.stdin.flush()
        reply = self.replies.get(timeout=60)
        assert reply is not None, "browser driver stopped before confirming its phase"
        assert reply.get("ok"), "browser phase " + action + " failed: " + reply.get("error", "unknown")
        return reply["result"]

    def close(self):
        forced = False
        if not self.child.stdin.closed:
            self.child.stdin.close()
        try:
            self.child.wait(timeout=15)
        except subprocess.TimeoutExpired:
            forced = True
            self.child.terminate()
            try:
                self.child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=3)
        assert not forced, "browser driver required forced termination instead of orderly shutdown"
        assert self.child.returncode == 0, "browser driver did not exit cleanly"


def sessions(console):
    result = console.request("amaterasu.authentication/1", {"action": "list_sessions", "offset": 0, "limit": 4})
    assert result.get("ok") and result.get("next_offset") is None, "native authentication metadata unavailable"
    return result["sessions"]


def worker_ready(console):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if any(line.startswith("Development worker ready: pid=") for line in console.errors):
            return
        assert console.process.poll() is None, "signed host stopped before native worker readiness: " + "".join(console.errors)
        time.sleep(0.05)
    raise AssertionError("signed native worker readiness deadline expired")


def exercise(directory, executable):
    harness = PlatformHarness(directory)
    private_directories = {
        "HOME": directory / "host-home",
        "XDG_CONFIG_HOME": directory / "xdg-config",
        "XDG_CACHE_HOME": directory / "xdg-cache",
        "XDG_DATA_HOME": directory / "xdg-data",
        "XDG_RUNTIME_DIR": directory / "xdg-runtime",
    }
    for private_directory in private_directories.values():
        private_directory.mkdir(mode=0o700)
    host_environment = os.environ.copy()
    host_environment.update({name: str(path) for name, path in private_directories.items()})
    certificate, spki, origin = configure_host(harness)
    harness.stage(harness.package(1))
    web = directory / "signed-web"
    run(harness.publisher, "pack-web", harness.root, WORKSPACE / "frontend/dist", web, "1")
    run(harness.publisher, "stage-web", harness.root, web)
    probe = RejectKeyProbe(directory / "unrelated-tls-key")
    console = None
    browser = None
    driver = None
    try:
        console = Console(harness, environment=host_environment)
        worker_ready(console)
        console.send({"op": "log", "level": 2, "target": "browser-test", "message": "prior support marker username=fixturealice source=192.0.2.13:1234 password=fixture-private-secret user_name=\"fixture private name\""})
        assert console.receive().get("kind") == "ok", "prior support record was rejected"
        stop_console(console)
        console.close()
        # The isolated host has initialized this database and is stopped. Seed
        # a bounded, synthetic history to exercise the production read route.
        history_database = harness.root / "data" / "state" / "session-history" / "history.db"
        assert history_database.exists(), "private history fixture database is unavailable"
        with sqlite3.connect(history_database) as database:
            database.execute(
                "INSERT INTO sessions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                ("signed-history-fixture", "sunshine-modern", "Fixture client", "Fixture device",
                 "Fixture game", 1920, 1080, 60, 18000, "hevc", 90, 500, 410),
            )
            database.executemany(
                "INSERT INTO samples (session_id,recorded_at_unix,frames_sent,bytes_sent,dropped_frames,actual_fps,actual_bitrate_kbps,encode_latency_ms) VALUES (?,?,?,?,?,?,?,?)",
                [("signed-history-fixture", 100 + index * 10, index * 60, index * 10000,
                  index, 60.0, 8000.0, 4.2 if index == 32 else None) for index in range(33)],
            )
            database.execute("INSERT INTO events (session_id,recorded_at_unix,event_type,payload) VALUES (?,?,?,?)",
                ("signed-history-fixture", 490, "stream_stopped", '{"state":"stopped"}'))
            database.executemany(
                "INSERT INTO sessions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                [("signed-group-old", "sunshine-modern", "Group client", "Group device",
                  "", 1920, 1080, 60, 18000, "hevc", 850, 950, 100),
                 ("signed-group-new", "sunshine-modern", "Group client", "Group device",
                  "", 1920, 1080, 60, 18000, "hevc", 1000, 1100, 100)],
            )
            database.executemany(
                "INSERT INTO samples (session_id,recorded_at_unix,frames_sent,bytes_sent,dropped_frames,actual_fps,actual_bitrate_kbps,encode_latency_ms) VALUES (?,?,?,?,?,?,?,?)",
                [(session_id, start + index * 10, index * 60, index * 10000, 0, 60.0, 8000.0, None)
                 for session_id, start in [("signed-group-old", 860), ("signed-group-new", 1010)]
                 for index in range(2)],
            )
        console = Console(harness, environment=host_environment)
        worker_ready(console)
        console.send({"op": "log", "level": 2, "target": "browser-test", "message": "signed browser host log marker"})
        assert console.receive().get("kind") == "ok", "fixture log record was rejected"
        assert ready(console)["revision"] == 0
        assert console.request("amaterasu.authentication/1", {
            "action": "setup", "username": "browser-administrator", "password": "private-browser-test-password",
        }).get("ok"), "private administrator setup failed"
        fixtures = WORKSPACE / "modules/pairing/tests/fixtures"
        pairing_identity = console.request("amaterasu.pairing/1", {
            "action": "import_identity", "certificate_pem": (fixtures / "host-cert.pem").read_text(),
            "private_key_pem": (fixtures / "host-key.pem").read_text(),
        })
        assert pairing_identity.get("ok"), "private pairing identity could not be imported"
        imported = console.request("amaterasu.pairing/1", {
            "action": "import_client", "client_id": "signed-browser-fixture", "name": "Browser test device",
            "certificate_pem": (WORKSPACE / "modules/pairing/tests/fixtures/client-cert.pem").read_text(),
        })
        assert imported.get("ok"), "private paired-device fixture could not be imported"
        from test_nvhttp import wait_for
        nvhttp = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda value: value.get("tls_ready"))
        nvhttp_origin = "https://" + nvhttp["https_address"]
        # Independently verify the host's actual certificate and loopback IP SAN.
        context = ssl.create_default_context(cafile=str(certificate))
        with socket.create_connection(("127.0.0.1", int(origin.rsplit(":", 1)[1])), timeout=4) as raw:
            with context.wrap_socket(raw, server_hostname="127.0.0.1"):
                pass
        browser = OwnedBrowser(directory / "aborted-browser", executable, spki)
        try:
            browser.start()
            raise RuntimeError("injected post-admission browser assertion")
        except RuntimeError as error:
            assert str(error) == "injected post-admission browser assertion"
            abort_cleanup = browser.close()
            assert abort_cleanup["retained_identities"] >= 2 and abort_cleanup["private_profile_processes_remaining"] == 0, "browser helper abort cleanup was not qualified"
            print("Browser abort cleanup:", json.dumps(abort_cleanup))
            browser = None
        browser = OwnedBrowser(directory / "remembered-browser", executable, spki)
        debugger = browser.start()
        driver = BrowserDriver()
        download_directory = directory / "browser-downloads"
        download_directory.mkdir()
        saved = driver.phase("initial", debugger=debugger, origin=origin, rejected_origin=probe.origin,
            profile=str(browser.profile), spki=spki,
            username="browser-administrator", password="private-browser-test-password",
            download_directory=str(download_directory), nvhttp_origin=nvhttp_origin)
        bundle = download_directory / "amaterasu-support-logs.zip"
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and (not bundle.exists() or list(download_directory.glob("*.crdownload"))):
            time.sleep(0.05)
        assert bundle.exists(), "browser support bundle was not downloaded"
        with zipfile.ZipFile(bundle) as archive:
            text = "\n".join(archive.read(name).decode("utf-8") for name in archive.namelist())
        assert "prior support marker" in text and "signed browser host log marker" in text, "support bundle omitted retained sessions"
        assert all(value not in text for value in ["fixturealice", "192.0.2.13", "fixture-private-secret", "fixture private name"]), "support bundle exposed fixture identifiers"
        history_export = download_directory / "amaterasu-session-history.json"
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and (not history_export.exists() or list(download_directory.glob("*.crdownload"))):
            time.sleep(0.05)
        assert history_export.exists(), "browser session history was not downloaded"
        exported = json.loads(history_export.read_text())
        assert exported["format"] == "amaterasu-session-history/1", "session export format is invalid"
        assert exported["scope"] == "retained_snapshot" and exported["detail"]["sample_count"] == 33 and len(exported["detail"]["samples"]) == 33, "session export omitted retained samples"
        assert not exported["detail"]["samples_truncated"] and exported["detail"]["samples"][-1]["encode_latency_ms"] == 4.2, "session export changed recorded samples"
        driver.phase("log_follow_start")
        console.send({"op": "log", "level": 2, "target": "browser-test", "message": "signed live follow marker"})
        assert console.receive().get("kind") == "ok", "live log record was rejected"
        driver.phase("log_follow_updated")
        console.send({"op": "log", "level": 2, "target": "browser-test", "message": "signed paused follow marker"})
        assert console.receive().get("kind") == "ok", "paused log record was rejected"
        driver.phase("log_follow_resume")
        paired = console.request("amaterasu.pairing/1", {"action": "list_clients"})
        assert paired.get("ok") and paired.get("clients") == [], "browser removal was not durable"
        previous = sessions(console)
        assert len(previous) == 1 and previous[0]["remember_me"], "UI remembered login did not create one native session"
        driver.phase("refresh")
        rotated = sessions(console)
        assert len(rotated) == 1 and rotated[0]["session_id"] != previous[0]["session_id"], "native session did not rotate"
        assert rotated[0]["refresh_expires_at"] == previous[0]["refresh_expires_at"], "refresh extended the remembered session deadline"
        stop_console(console)
        console = Console(harness, environment=host_environment)
        worker_ready(console)
        restored = ready(console)
        assert restored["revision"] == saved["revision"] and restored["values"]["origin_web_ui_allowed"] == saved["selection"], "native restart lost the browser policy save"
        driver.phase("after_restart")
        current = sessions(console)
        assert len(current) == 1 and current[0]["session_id"] == rotated[0]["session_id"], "native restart lost remembered session identity"
        # Restart Chromium itself with the same private profile. The remembered
        # refresh cookie must restore browser authority in a fresh process after
        # the native host has independently restarted.
        profile = browser.profile
        browser_owner_directory = browser.directory
        prior_identity = next(identity for identity in browser.descriptors if identity[0] == browser.child.pid)
        driver.phase("prepare_cold_restart")
        driver.close()
        driver = None
        browser.close()
        browser = None
        browser = OwnedBrowser(browser_owner_directory, executable, spki, reuse_profile=True)
        assert browser.profile == profile, "cold browser restart changed the private profile"
        debugger = browser.start()
        next_identity = next(identity for identity in browser.descriptors if identity[0] == browser.child.pid)
        assert next_identity != prior_identity, "cold browser restart reused the prior process identity"
        driver = BrowserDriver()
        driver.phase("cold_restart", debugger=debugger, origin=origin, profile=str(profile), spki=spki)
        cold_sessions = sessions(console)
        assert len(cold_sessions) == 1 and cold_sessions[0]["session_id"] != rotated[0]["session_id"], "cold browser restart did not rotate the native session"
        assert cold_sessions[0]["refresh_expires_at"] == rotated[0]["refresh_expires_at"], "cold browser refresh extended the remembered session deadline"
        revoked = console.request("amaterasu.authentication/1", {"action": "revoke_session", "session_id": cold_sessions[0]["session_id"]})
        assert revoked.get("ok"), "trusted-console session revocation failed"
        assert sessions(console) == [], "trusted-console revocation did not remove the native session"
        driver.phase("revoked")
        assert sessions(console) == [], "revoked browser session remained durable"
        driver.phase("logout", username="browser-administrator", password="private-browser-test-password")
        assert sessions(console) == [], "UI logout retained a durable native session"
    finally:
        try:
            if driver is not None:
                driver.close()
        finally:
            try:
                if browser is not None:
                    print("Browser final cleanup:", json.dumps(browser.close()))
            finally:
                try:
                    if console is not None:
                        try:
                            stop_console(console)
                        finally:
                            console.close()
                finally:
                    probe.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true")
    arguments = parser.parse_args()
    assert os.uname().sysname == "Linux" and os.geteuid() != 0, "run this native browser gate as a normal Linux user"
    executable = shutil.which(os.environ.get("AMATERASU_BROWSER", "chromium"))
    assert executable, "Chromium is required for the signed native browser gate"
    if not arguments.skip_build:
        run(CARGO, "build", "--workspace", "--locked", "--offline")
        run("npm", "--prefix", WORKSPACE / "frontend", "run", "build")
    with tempfile.TemporaryDirectory(prefix="amaterasu-signed-browser-") as temporary:
        exercise(Path(temporary), executable)
    print("Passed: actual Chromium with signed native HTTPS/frontend, remembered UI login/reload and cold-process restoration from the persistent cookie, HttpOnly cookie secrecy, exact CSRF checks, native refresh/revocation, scoped token creation/use/revocation, session-only administration and peer-session revocation after refresh, UI CAS/stale editor, client permission mode and grant changes with paired TLS list/launch denial and restoration, host restart and peer signout; SPKI-key exception negative control and owned browser abort cleanup.")
    print("Unqualified: installed CA/service TLS provisioning, physical remote browsers, Windows/macOS and game-visible streaming.")


if __name__ == "__main__":
    main()
