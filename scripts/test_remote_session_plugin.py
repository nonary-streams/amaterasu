#!/usr/bin/env python3
"""Qualify signed remote-session plugins in a private loopback host.

Uses real remote-session, black capture, video, software x264, RTSP, ENet and
encrypted UDP components. The signed input implementation is replaced by the
in-memory input-probe fixture before the first host process starts. Display,
desktop capture and audio providers remain disabled. Only a freshly generated
test publisher key is used; no installed host, service, key or firewall changes.

Build the product libraries and amaterasu-input-probe with
amaterasu-encode/software-x264, and the interactive ENet oracle, separately.
Pass their artifact directory explicitly. Native monitor behavior and a real
Moonlight player remain outside this qualification.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import queue
import secrets
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.parse import urlsplit

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

from test_host_service import Console, wait_gone, worker_pid
from test_linux_session_profile import isolated_environment
from test_management_api import Api, ORIGIN, PASSWORD, available_moonlight_port_pair
from test_nvhttp import get, pair, successful, tls_context, wait_for
from test_public_launch import launch_query
from test_public_software_stream import start_control
from test_rtsp import PeerDiscovery, RtspClient, announcement
from test_stream_input import input_packet, keyboard, send_input
from test_stream_session import collect, stop_client


SOURCE_ROOT = Path(__file__).resolve().parents[1]
SESSION_SERVICE = "nonary.session-presentation/1"
PLUGIN = "amaterasu.remote-session"
INPUT_METRICS = "test.input-probe/1"
WIDTH, HEIGHT, FPS = 64, 48, 30
MODE = {"width": WIDTH, "height": HEIGHT, "refresh_millihz": FPS * 1000, "hdr": False}


def require(condition: bool, message: str, detail: object = None) -> None:
    if not condition:
        raise AssertionError(message if detail is None else f"{message}: {detail}")


def wait_until(observe, predicate, message: str, seconds: float = 15):
    deadline = time.monotonic() + seconds
    value = None
    while time.monotonic() < deadline:
        value = observe()
        if predicate(value):
            return value
        time.sleep(0.02)
    raise AssertionError(f"{message}: {value}")


class NativeConsole(Console):
    """The development console retains exact API admission/result semantics."""

    def receive(self):
        try:
            reply = self.replies.get(timeout=30)
        except queue.Empty as error:
            raise AssertionError("native host reply timed out\n" + "".join(self.errors)) from error
        require(reply is not None, "native host exited", "".join(self.errors))
        return reply

    def command(self, command):
        self.send(command)
        return self.receive()

    def api(self, method, arguments, *, major=1):
        self.sequence += 1
        admitted = self.command({
            "op": "call_api", "service": SESSION_SERVICE, "method": method,
            "interface_major": major, "interface_minor": 0, "payload": arguments,
            "timeout_ms": 15_000, "operation_id": f"remote-native-api-{self.sequence}",
        })
        if admitted.get("kind") != "accepted":
            return admitted
        request_id = admitted["id"]
        result = wait_until(
            lambda: self.command({"op": "take_result", "request": request_id}),
            lambda value: value.get("kind") != "pending", "API result was not consumed",
        )
        require(result.get("kind") == "result", "approved API request failed", result)
        return result["payload"]

    def settled_api(self, method, arguments):
        return wait_until(lambda: self.api(method, arguments),
                          lambda value: value.get("result") != "pending", "native API operation did not settle")


@dataclass
class Fixture:
    sdk_root: Path
    directory: Path
    artifacts: Path
    root: Path
    package: Path
    environment: dict[str, str]

    @property
    def host(self):
        return self.artifacts / "amaterasu"

    @property
    def publisher(self):
        return self.artifacts / "amaterasu-release"

    def run(self, *arguments, success=True):
        result = subprocess.run(
            [str(value) for value in arguments], cwd=self.sdk_root,
            text=True, capture_output=True, timeout=180,
        )
        require((result.returncode == 0) is success, "fixture command failed",
                {"arguments": [str(value) for value in arguments], "stdout": result.stdout,
                 "stderr": result.stderr, "exit": result.returncode})
        return result.stdout


def approve_fixture_console(policy):
    calls = policy["principals"]["amaterasu.console"].setdefault("call", [])
    for service in (SESSION_SERVICE, INPUT_METRICS, "amaterasu.rtsp/1"):
        if service not in calls:
            calls.append(service)
    for approval in policy.get("api_methods", []):
        if approval["method"]["service"] == SESSION_SERVICE:
            if "amaterasu.console" not in approval["callers"]:
                approval["callers"].append("amaterasu.console")
    remote = policy["settings"][PLUGIN]
    remote["allowed_principals"] = ["amaterasu.nvhttp", "amaterasu.console"]
    remote["source_verifiers"] = ["amaterasu.nvhttp", "amaterasu.console"]


def configure_fixture(policy, base_port):
    settings = policy["settings"]
    approve_fixture_console(policy)
    settings["amaterasu.pairing"]["host_configuration"] = False
    settings["amaterasu.management"] = {
        "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
    }
    settings["amaterasu.nvhttp"].update({
        "enabled": True, "http_bind": f"127.0.0.1:{base_port}",
        "https_bind": f"127.0.0.1:{base_port - 5}", "allow_remote": False,
        "session_presentation_provider": SESSION_SERVICE,
        "auxiliary_black_capture_provider": "black",
        "auxiliary_exact_capture_provider": "vkms",
        "streaming": {
            "capture": {"capture_provider": "black", "output": f"black:{WIDTH}x{HEIGHT}@{FPS}:nv12",
                        "encoder": "auto", "audio_endpoint": "default"},
            "profile": {"max_width": WIDTH, "max_height": HEIGHT, "max_fps": FPS,
                        "bitrate_kbps": 1000, "max_reference_frames": 1,
                        "packet_size": 1024, "fec_percentage": 20, "min_parity_shards": 2,
                        "max_fec_blocks": 1, "packet_ms": 10, "audio_bitrate": 128000,
                        "encrypt_video": True, "encrypt_audio": True,
                        "permit_legacy_audio_iv_wrap": False},
        },
    })
    settings["amaterasu.rtsp"].update({
        "enabled": True, "bind": f"127.0.0.1:{base_port + 21}",
        "allow_remote": False, "allowed_principals": ["amaterasu.nvhttp"],
        "retirement_status_principals": [PLUGIN, "amaterasu.console"],
        "retirement_status_owners": {
            PLUGIN: "amaterasu.nvhttp", "amaterasu.console": "amaterasu.nvhttp",
        },
    })
    settings["amaterasu.stream-session"].update({
        "enabled": True, "enable_input": True,
        "allowed_principals": ["amaterasu.rtsp"], "input_lease_ttl_ms": 1000,
    })
    for name in ("amaterasu.media-transport", "amaterasu.control-transport"):
        settings[name] = {"enabled": True, "allow_remote": False,
                          "allowed_principals": ["amaterasu.stream-session"]}
    settings["amaterasu.input"] = {"enabled": True}
    settings["amaterasu.display"]["allow_mutation"] = False
    settings["amaterasu.virtual-display"]["enabled"] = False
    settings["amaterasu.audio"] = {"enabled": False}
    settings["amaterasu.audio-encode"] = {"enabled": False}
    for name in ("amaterasu.capture-kms", "amaterasu.capture-pipewire", "amaterasu.capture-vkms"):
        settings[name]["enabled"] = False
    settings["amaterasu.linux-session-video"] = {
        "enabled": True, "capture_providers": [],
        "source_preflight_routes": {"synthetic_black": {"provider": "black", "class": "synthetic"}},
        "preflight_only_principals": [PLUGIN],
        "external_capture_providers": {
            "black": {"service": "amaterasu.capture-black/1", "frame_transport": "generic_cpu"},
        },
    }
    settings["amaterasu.capture-black"] = {"enabled": True}
    settings["amaterasu.encode"] = {"enabled": True}
    policy["principals"]["amaterasu.input"]["provide"].append(INPUT_METRICS)
    assert_device_isolation(policy)


def assert_device_isolation(policy):
    settings = policy["settings"]
    require(settings["amaterasu.display"]["allow_mutation"] is False, "physical display mutation enabled")
    for name in ("amaterasu.virtual-display", "amaterasu.audio", "amaterasu.audio-encode",
                 "amaterasu.capture-kms", "amaterasu.capture-pipewire", "amaterasu.capture-vkms"):
        require(settings[name]["enabled"] is False, "native device provider enabled", name)
    require(settings["amaterasu.linux-session-video"]["capture_providers"] == [],
            "a desktop capture provider was selected")
    require(settings["amaterasu.linux-session-video"]["external_capture_providers"] == {
        "black": {"service": "amaterasu.capture-black/1", "frame_transport": "generic_cpu"}},
        "capture is not restricted to the explicit black source")
    require(settings["amaterasu.linux-session-video"]["source_preflight_routes"] == {
        "synthetic_black": {"provider": "black", "class": "synthetic"}},
        "preflight can select a managed or desktop source")
    require(settings["amaterasu.linux-session-video"]["preflight_only_principals"] == [PLUGIN],
            "remote provider is not limited to read-only Video preflight")
    require(settings["amaterasu.input"] == {"enabled": True}, "input fixture settings changed")
    rtsp = settings["amaterasu.rtsp"]
    require(rtsp["allowed_principals"] == ["amaterasu.nvhttp"],
            "fixture observers gained RTSP open or close authority")
    require(rtsp["retirement_status_owners"] == {
        PLUGIN: "amaterasu.nvhttp", "amaterasu.console": "amaterasu.nvhttp"},
        "retirement observations are not scoped to the NVHTTP transport owner")
    for name in ("amaterasu.nvhttp", "amaterasu.rtsp", "amaterasu.media-transport", "amaterasu.control-transport"):
        require(settings[name].get("allow_remote", False) is False, "remote listener enabled", name)


def replace_input_manifest(manifest, artifact):
    components = manifest["components"]
    selected = [entry for entry in components if entry["id"] == "amaterasu.input"]
    require(len(selected) == 1, "signed set must have one input implementation")
    component = selected[0]
    component.update(file=artifact.name, sha256=hashlib.sha256(artifact.read_bytes()).hexdigest(),
                     size=artifact.stat().st_size)
    component["requested"].setdefault("provide", []).append(INPUT_METRICS)
    require(component["file"] == "libamaterasu_input_probe.so", "real input library was not replaced")


def sign_test_manifest(root, package, manifest):
    """Sign only a development root with its own approved ephemeral test key."""
    bootstrap = json.loads((root / "bootstrap.json").read_text())
    require(bootstrap.get("development") is True, "test signer requires an isolated development root")
    secret = bytes.fromhex(json.loads((root / "development-key.json").read_text()))
    key = Ed25519PrivateKey.from_private_bytes(secret)
    public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
    require(bootstrap["trusted_keys"].get("local-development") == public,
            "fixture key is not the root's approved development key")
    raw = json.dumps(manifest, indent=2, ensure_ascii=False).encode()
    (package / "manifest.json").write_bytes(raw)
    (package / "manifest.sig").write_text(json.dumps({
        "key_id": "local-development", "signature": key.sign(raw).hex(),
    }))


def create_fixture(sdk_root, directory, artifacts, *, omit_plugin=False):
    directory.mkdir(mode=0o700)
    fixture = Fixture(sdk_root, directory, artifacts, directory / "root", directory / "package",
                      isolated_environment(directory))
    fixture.run(fixture.publisher, "init-linux-session", fixture.root)
    path = fixture.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    require(PLUGIN in bootstrap["policy"]["principals"], "release tool lacks remote-session composition")
    configure_fixture(bootstrap["policy"], available_moonlight_port_pair())
    path.write_text(json.dumps(bootstrap))
    fixture.run(fixture.publisher, "pack-linux-session", fixture.root, artifacts, fixture.package, "1")
    manifest_path = fixture.package / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    by_id = {entry["id"]: entry for entry in manifest["components"]}
    require(PLUGIN in by_id and "amaterasu.capture-black" in by_id,
            "signed profile does not select both independent plugins")
    probe = artifacts / "libamaterasu_input_probe.so"
    replace_input_manifest(manifest, probe)
    shutil.copy2(probe, fixture.package / probe.name)
    if omit_plugin:
        manifest["components"] = [entry for entry in manifest["components"] if entry["id"] != PLUGIN]
        for entry in manifest["components"]:
            entry["requires"].pop(PLUGIN, None)
    sign_test_manifest(fixture.root, fixture.package, manifest)
    fixture.run(fixture.publisher, "verify", fixture.root, fixture.package)
    fixture.run(fixture.publisher, "stage", fixture.root, fixture.package)
    return fixture


def wait_ready(console, seconds=120):
    wait_until(lambda: (console.process.poll(), tuple(console.errors)),
               lambda value: value[0] is not None or any("Development worker ready: pid=" in line for line in value[1]),
               "signed native worker did not become ready", seconds)
    require(console.process.poll() is None, "signed native worker failed to load", "".join(console.errors))


def context(client="fixture-secondary", *, game=True, launch=True, view=True, list_apps=True):
    return {"client": {"id": client, "paired": True, "may_view": view, "may_list": list_apps,
                       "may_launch": launch, "may_terminate": True},
            "game": {"application": {"id": "42", "uuid": "fixture-game-uuid", "title": "Fixture game",
                                     "aliases": [], "visible": True, "monitor_profile": False, "properties": {}},
                     "owner_client_id": "fixture-owner", "generation": 7} if game else None}


def qualify_loaded_contract(console):
    described = console.api("describe", {})
    require(described == {"result": "described", "interface_major": 1, "interface_minor": 0},
            "loaded plugin did not describe the exact SDK", described)
    wrong_version = console.api("describe", {}, major=2)
    require(wrong_version.get("kind") == "error", "broker accepted a different interface identity", wrong_version)
    direct = console.command({"op": "request", "service": SESSION_SERVICE, "payload": {"action": "describe"},
                              "timeout_ms": 1000, "operation_id": "remote-raw-route-must-not-bypass-api"})
    require(direct.get("kind") == "error" or direct.get("payload", {}).get("result") == "error",
            "raw request bypassed registered API schema", direct)
    owner = console.settled_api("info", {"context": context("fixture-owner")})
    secondary = console.settled_api("info", {"context": context()})
    require(owner["presentation"]["busy"] is True and owner["presentation"]["current_game"] == "42",
            "owner view lost the running application", owner)
    require(secondary["presentation"]["busy"] is False and secondary["presentation"]["current_game"] is None,
            "secondary view did not project a selectable catalog", secondary)
    started = console.settled_api("begin_project", {"context": context(), "catalog_revision": "native-fixture-revision"})
    projection = started["projection_id"]
    appended = console.api("append_project", {"client_id": "fixture-secondary", "projection_id": projection,
                            "entries": [context()["game"]["application"]], "final_page": True})
    require(appended == {"result": "appended", "complete": True}, "catalog append failed", appended)
    projected = console.api("read_project", {"client_id": "fixture-secondary", "projection_id": projection,
                            "offset": 0, "limit": 32})
    titles = [entry["title"].strip() for entry in projected["entries"]]
    require({"Remote Input", "Remote Monitor", "Resume", "Terminate", "Fixture game"} <= set(titles),
            "loaded plugin did not project protocol controls", projected)
    console.api("release_project", {"client_id": "fixture-secondary", "projection_id": projection})
    released = console.api("read_project", {"client_id": "fixture-secondary", "projection_id": projection,
                           "offset": 0, "limit": 32})
    require(released["result"] == "error", "released projection remained readable", released)
    denied = console.settled_api("dispatch", {"context": context(game=False, launch=False), "operation_id": "denied-input",
                         "intent": "launch", "selection": {"id": "2147483506", "uuid": None}, "mode": MODE})
    require(denied.get("decision", {}).get("kind") == "reject"
            and denied["decision"]["status_code"] == 403, "launch permission was bypassed", denied)
    permission_split = qualify_catalog_permission_split(console)
    return {"interface": "1.0", "projected_entries": len(projected["entries"]),
            "owner_busy": True, "secondary_busy": False, "projection_released": True,
            "catalog_permissions": permission_split}


def qualify_catalog_permission_split(console):
    """List authorizes rows independently of the right to view a stream."""
    for name, view, list_apps in (("list-only", False, True), ("view-only", True, False)):
        client_id = f"fixture-{name}"
        started = console.settled_api("begin_project", {
            "context": context(client_id, game=False, launch=False, view=view, list_apps=list_apps),
            "catalog_revision": f"native-{name}-revision",
        })
        require(started.get("result") == "started", "trusted inventory upload was not admitted", started)
        projection = started["projection_id"]
        try:
            appended = console.api("append_project", {
                "client_id": client_id, "projection_id": projection, "entries": [], "final_page": True,
            })
            require(appended.get("result") == "appended", "trusted inventory upload did not complete", appended)
            rows = console.api("read_project", {
                "client_id": client_id, "projection_id": projection, "offset": 0, "limit": 32,
            })
            if list_apps:
                require(rows.get("result") == "projected", "List-only client could not read its catalog", rows)
            else:
                require(rows.get("result") == "error" and "entries" not in rows,
                        "View-only client received catalog rows without List permission", rows)
        finally:
            console.api("release_project", {"client_id": client_id, "projection_id": projection})
    return {"list_only_can_read": True, "view_only_rows_denied": True}


def initialize_paired_wire(console, fixtures, *, select_software=True):
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_identity", "certificate_pem": (fixtures / "host-cert.pem").read_text(),
        "private_key_pem": (fixtures / "host-key.pem").read_text(),
    })
    require(imported.get("ok") is True, "test TLS identity import failed", imported)
    state = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda result: result.get("tls_ready"))
    setup = console.request("amaterasu.authentication/1", {
        "action": "setup", "username": "administrator", "password": PASSWORD,
    })
    require(setup.get("ok") is True, "isolated account setup failed", setup)
    api = Api(console)
    status, login, _ = api.post("/v1/auth/login", {"username": "administrator", "password": PASSWORD})
    require(status == 200 and login.get("ok"), "isolated management login failed", login)
    bearer = login["bearer"]
    if select_software:
        status, settings, _ = api.post("/v1/nvhttp/settings", bearer=bearer)
        require(status == 200 and settings.get("ok"), "NVHTTP settings unavailable", settings)
        status, updated, _ = api.post("/v1/nvhttp/settings_update", {
            "hostname": settings["hostname"], "port": settings["port"], "expected_revision": settings["revision"],
            "lan_encryption_mode": 2, "encoder": "software", "sw_preset": "superfast", "sw_tune": "zerolatency",
            "min_threads": 2,
        }, bearer)
        require(status == 200 and updated.get("ok"), "software encoder selection failed", updated)
        status, encoders, _ = api.post("/v1/media/encoders", bearer=bearer)
        require(status == 200 and encoders.get("software_encoder", {}).get("available") is True,
                "artifact needs amaterasu-encode/software-x264 and compatible libx264", encoders)
    http, https = "http://" + state["http_address"], "https://" + state["https_address"]
    pair(console, http)
    return https, tls_context()


def complete_h264_units(datagrams, key):
    """Independently extract complete data shards; parity is not needed here."""
    frames = {}
    for datagram in datagrams:
        require(len(datagram) >= 64, "truncated encrypted video datagram")
        packet = AESGCM(key).decrypt(datagram[:12], datagram[32:] + datagram[16:32], None)
        require(packet[:2] == b"\x90\0" and len(packet) >= 40, "invalid RTP video header")
        frame = int.from_bytes(packet[20:24], "little")
        fec = int.from_bytes(packet[28:32], "little")
        index, count = (fec >> 12) & 1023, fec >> 22
        require(0 < count <= 1023, "invalid data-shard count")
        entry = frames.setdefault(frame, {"count": count, "shards": {}})
        require(entry["count"] == count, "inconsistent frame shard count")
        if index < count:
            require(index not in entry["shards"], "duplicate authenticated data shard")
            entry["shards"][index] = packet[32:]
    units = []
    for _, entry in sorted(frames.items()):
        if len(entry["shards"]) != entry["count"]:
            continue
        payload = b"".join(entry["shards"][index] for index in range(entry["count"]))
        require(payload[0] == 1, "invalid GameStream access-unit header")
        final_size = int.from_bytes(payload[4:6], "little")
        shard_size = len(entry["shards"][0])
        require(0 < final_size <= shard_size, "invalid access-unit final size")
        length = (entry["count"] - 1) * shard_size + final_size
        unit = payload[8:length]
        require(unit.startswith(b"\0\0\0\1"), "access unit is not Annex B H.264")
        units.append(unit)
    require(len(units) >= 3, "fewer than three complete encoded black frames", len(units))
    return units


def decode_black(units, directory):
    path = directory / "remote-input.h264"
    path.write_bytes(b"".join(units))
    decoded = subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "error", "-threads", "1", "-f", "h264", "-i", str(path),
        "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1",
    ], capture_output=True, timeout=20)
    require(decoded.returncode == 0, "independent black-video decode failed", decoded.stderr.decode(errors="replace"))
    require(len(decoded.stdout) >= 3 * WIDTH * HEIGHT and len(decoded.stdout) % (WIDTH * HEIGHT) == 0,
            "decoded geometry differs from the requested black source", len(decoded.stdout))
    require(max(decoded.stdout) <= 4, "Remote Input captured non-black host pixels", max(decoded.stdout))
    return len(decoded.stdout) // (WIDTH * HEIGHT)


def retirement_snapshot(console):
    return console.request("amaterasu.rtsp/1", {"action": "retirement_status"})


def retired(value):
    return value.get("ok") is True and value.get("startup_reconciled") is True \
        and value.get("active_operations") == 0 and value.get("pending_admissions") == 0 \
        and value.get("cleanup_pending") is False


def exercise_remote_input(console, https, tls, oracle, directory):
    listing = successful(get(https, "/applist", {"uniqueid": "query-id-does-not-authorize"}, tls))
    controls = {entry.findtext("AppTitle", "").strip(): entry.findtext("ID") for entry in listing.findall("App")}
    require("Remote Input" in controls and "Remote Monitor" in controls, "signed wire omitted remote controls", controls)
    before = console.request(INPUT_METRICS, {"action": "status"})
    require(before.get("fixture") is True and before["active"] is False, "selected input provider is not the idle fixture", before)
    info = successful(get(https, "/serverinfo", {"uniqueid": "forged-other-client"}, tls))
    require(info.findtext("currentgame") == "0", "idle client was advertised as a running application")
    key = secrets.token_bytes(16)
    opened = successful(get(https, "/launch", {
        **launch_query(key, controls["Remote Input"]), "mode": f"{WIDTH}x{HEIGHT}x{FPS}",
    }, tls, timeout=40))
    url = opened.findtext("sessionUrl0")
    require(url and url.startswith("rtspenc://"), "auxiliary launch did not create encrypted RTSP", opened.attrib)
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        association = PeerDiscovery(video, audio)
        parsed = urlsplit(url)
        client = RtspClient(f"{parsed.hostname}:{parsed.port}", url, key)
        ports = client.setup(association.on_setup)
        sdp = announcement().replace(b"clientViewportWd:16", f"clientViewportWd:{WIDTH}".encode())
        sdp = sdp.replace(b"clientViewportHt:16", f"clientViewportHt:{HEIGHT}".encode())
        client.transact("ANNOUNCE", "streamid=control/13/0", {"Content-Type": "application/sdp"}, sdp)
        client.transact("PLAY", "/")
        control = start_control(oracle, directory, ports["control"], key, ports["connect_data"])
        try:
            association.send("video")
            active = wait_until(lambda: console.request(INPUT_METRICS, {"action": "status"}),
                                lambda value: value.get("active") is True, "input fixture never acquired its lease")
            require(active["acquired"] == before["acquired"] + 1, "auxiliary stream did not acquire exactly one input lease", active)
            send_input(control, key, 1, keyboard(True))
            send_input(control, key, 2, input_packet(7, struct.pack(">hh", -7, 13)))
            send_input(control, key, 3, input_packet(8, b"\x01"))
            held = wait_until(lambda: console.request(INPUT_METRICS, {"action": "status"}),
                              lambda value: value["held_keys"] == 1 and value["held_buttons"] == 1,
                              "authenticated control did not reach in-memory input")
            require(held["events"][-3:] == [
                {"kind": "key", "key": {"platform": "moonlight_vk", "code": 65}, "state": "down"},
                {"kind": "relative", "dx": -7, "dy": 13},
                {"kind": "button", "button": "left", "pressed": True},
            ], "remote input was translated incorrectly", held)
            # A measured interval supplies several paced black frames. The
            # timeout is bounded and no remote client presentation is inferred.
            deadline = time.monotonic() + 0.4
            while time.monotonic() < deadline:
                time.sleep(0.02)
            cancelled = successful(get(https, "/cancel", {"uniqueid": "forged-query-id"}, tls, timeout=40))
            require(cancelled.findtext("cancel") == "1", "paired auxiliary cancel was not acknowledged", cancelled.attrib)
            cleanup = wait_until(lambda: console.request(INPUT_METRICS, {"action": "status"}),
                                 lambda value: value.get("active") is False, "input lease survived cancel")
            require(cleanup["held_keys"] == cleanup["held_buttons"] == 0
                    and cleanup["released"] == before["released"] + 1 and cleanup["expired"] == before["expired"],
                    "cancel did not explicitly retire held input", cleanup)
            proof = wait_until(lambda: retirement_snapshot(console), retired,
                               "RTSP did not confirm native retirement", seconds=30)
            datagrams = collect(video, ("127.0.0.1", ports["video"]))
            require(collect(audio, ("127.0.0.1", ports["audio"])) == [], "Remote Input opened an audio media path")
            frames = decode_black(complete_h264_units(datagrams, key), directory)
            return {"decoded_black_frames": frames, "input_acquired": 1, "input_released": 1,
                    "held_input_neutralized": True, "rtsp_retirement": proof}
        finally:
            stop_client(control)
            association.close()


def reject_unavailable_monitor(console):
    arguments = {"context": context("disabled-monitor-client", game=False), "operation_id": "disabled-monitor-request",
                 "intent": "launch", "selection": {"id": "2147483505", "uuid": None}, "mode": MODE}
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline:
        response = console.api("dispatch", arguments)
        require(response.get("result") in ("pending", "error")
                or response.get("decision", {}).get("kind") == "reject",
                "disabled monitor provider advertised a capture-ready source", response)
        if response.get("result") != "pending":
            break
        time.sleep(0.02)
    cancelled = console.settled_api("cancel_dispatch", {
        "client_id": "disabled-monitor-client", "operation_id": "disabled-monitor-request",
    })
    require(cancelled.get("result") == "acknowledged", "failed monitor admission retained uncertain ownership", cancelled)
    return {"native_display_disabled": True, "never_capture_ready": True, "admission_cancelled": True}


def qualify_missing_provider(fixture, fixtures):
    console = NativeConsole(fixture, environment=fixture.environment)
    try:
        try:
            wait_ready(console, seconds=30)
        except AssertionError:
            require(console.process.poll() is not None or not any(
                "Development worker ready: pid=" in line for line in console.errors),
                "unexpected readiness state after missing-provider failure")
            return {"selected_provider_missing": True, "host_failed_closed": True}
        https, tls = initialize_paired_wire(console, fixtures, select_software=False)
        status, response = get(https, "/applist", {"uniqueid": "missing-plugin-client"}, tls, timeout=40)
        require(status != 200 or response.get("status_code") != "200",
                "selected missing provider silently fell back to an ordinary catalog", response.attrib)
        return {"selected_provider_missing": True, "wire_status": int(response.get("status_code", status))}
    finally:
        if console.process.poll() is None:
            console.process.stdin.close()
            console.process.wait(timeout=15)
        (fixture.directory / "host.stderr.log").write_text("".join(console.errors))
        console.close()


class HarnessSelfTests(unittest.TestCase):
    def test_input_substitution_preserves_identity_and_uses_exact_fixture_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "libamaterasu_input_probe.so"
            artifact.write_bytes(b"in-memory input implementation")
            manifest = {"components": [{"id": "amaterasu.input", "file": "libamaterasu_input.so",
                                        "requested": {"provide": ["amaterasu.input/1"]},
                                        "requires": {"amaterasu.runtime": "0.1.0"}}]}
            replace_input_manifest(manifest, artifact)
            selected = manifest["components"][0]
            self.assertEqual(selected["id"], "amaterasu.input")
            self.assertEqual(selected["sha256"], hashlib.sha256(artifact.read_bytes()).hexdigest())
            self.assertIn(INPUT_METRICS, selected["requested"]["provide"])

    def test_signing_requires_the_roots_own_approved_development_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            key = Ed25519PrivateKey.generate()
            secret = key.private_bytes(serialization.Encoding.Raw, serialization.PrivateFormat.Raw,
                                       serialization.NoEncryption())
            public = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex()
            (root / "development-key.json").write_text(json.dumps(secret.hex()))
            (root / "bootstrap.json").write_text(json.dumps({"development": True,
                                                             "trusted_keys": {"local-development": public}}))
            sign_test_manifest(root, root, {"components": []})
            signature = json.loads((root / "manifest.sig").read_text())
            key.public_key().verify(bytes.fromhex(signature["signature"]), (root / "manifest.json").read_bytes())
            (root / "bootstrap.json").write_text(json.dumps({"development": False, "trusted_keys": {}}))
            with self.assertRaises(AssertionError):
                sign_test_manifest(root, root, {"components": []})

    def test_isolation_guard_rejects_a_live_desktop_capture_provider(self):
        policy = {"settings": {"amaterasu.display": {"allow_mutation": True}}}
        with self.assertRaisesRegex(AssertionError, "physical display mutation"):
            assert_device_isolation(policy)

    def test_fixture_preflight_and_retirement_observers_keep_their_exact_scope(self):
        policy = {
            "principals": {"amaterasu.console": {"call": []}, "amaterasu.input": {"provide": []}},
            "settings": {name: {} for name in (
                PLUGIN, "amaterasu.pairing", "amaterasu.nvhttp", "amaterasu.rtsp",
                "amaterasu.stream-session", "amaterasu.display", "amaterasu.virtual-display",
                "amaterasu.capture-kms", "amaterasu.capture-pipewire", "amaterasu.capture-vkms")},
        }
        configure_fixture(policy, 47989)
        video = policy["settings"]["amaterasu.linux-session-video"]
        video["source_preflight_routes"]["managed_output"] = {"provider": "black", "class": "managed"}
        with self.assertRaisesRegex(AssertionError, "preflight can select"):
            assert_device_isolation(policy)
        del video["source_preflight_routes"]["managed_output"]
        video["preflight_only_principals"] = []
        with self.assertRaisesRegex(AssertionError, "read-only Video preflight"):
            assert_device_isolation(policy)
        video["preflight_only_principals"] = [PLUGIN]
        policy["settings"]["amaterasu.rtsp"]["allowed_principals"].append("amaterasu.console")
        with self.assertRaisesRegex(AssertionError, "open or close authority"):
            assert_device_isolation(policy)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk-root", type=Path, default=SOURCE_ROOT)
    parser.add_argument("--artifacts", type=Path, help="directory containing prebuilt host and native libraries")
    parser.add_argument("--oracle", type=Path, default=os.environ.get("AMATERASU_ENET_CLIENT_ORACLE"))
    parser.add_argument("--work-root", type=Path, help="absolute build-volume directory for private temporary hosts")
    parser.add_argument("--keep", action="store_true", help="retain the private successful qualification directory")
    parser.add_argument("--loader-only", action="store_true", help="qualify native API loading without the media leg")
    parser.add_argument("--self-test", action="store_true", help="check the fixture/signing safety helpers only")
    arguments = parser.parse_args()
    if arguments.self_test:
        result = unittest.main(argv=[sys.argv[0]], exit=False)
        if not result.result.wasSuccessful():
            raise SystemExit(1)
        return
    if sys.platform != "linux":
        parser.error("this fixture uses the Linux session package; native Windows qualification is separate")
    if arguments.artifacts is None or arguments.work_root is None:
        parser.error("provide --artifacts and an absolute --work-root on the build volume")
    if not arguments.work_root.is_absolute():
        parser.error("--work-root must be absolute")
    sdk_root = arguments.sdk_root.resolve(strict=True)
    artifacts = arguments.artifacts.resolve(strict=True)
    required = ["amaterasu", "amaterasu-release", "libamaterasu_input_probe.so",
                "libamaterasu_remote_session.so", "libamaterasu_capture_black.so"]
    require(all((artifacts / name).is_file() for name in required), "required native artifacts are missing", required)
    oracle = None if arguments.loader_only else Path(arguments.oracle or "").resolve(strict=True)
    if not arguments.loader_only:
        require(oracle.is_file() and shutil.which("ffmpeg") is not None,
                "full conformance requires an interactive ENet oracle and FFmpeg")
    arguments.work_root.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="remote-session-native-", dir=arguments.work_root))
    directory.chmod(0o700)
    fixtures = sdk_root / "modules/pairing/tests/fixtures"
    # Imported wire helpers must use the explicitly selected SDK's fixtures.
    import test_nvhttp
    import test_rtsp
    test_nvhttp.FIXTURES = fixtures
    test_rtsp.WORKSPACE = sdk_root
    evidence = {}
    succeeded = False
    try:
        fixture = create_fixture(sdk_root, directory / "normal", artifacts)
        console = NativeConsole(fixture, environment=fixture.environment)
        try:
            wait_ready(console)
            pid = worker_pid(console)
            evidence["native_contract"] = qualify_loaded_contract(console)
            if not arguments.loader_only:
                https, tls = initialize_paired_wire(console, fixtures)
                evidence["remote_input"] = exercise_remote_input(console, https, tls, oracle, directory)
            evidence["monitor_refusal"] = reject_unavailable_monitor(console)
            console.eof()
            wait_gone(pid)
        finally:
            (directory / "host.stderr.log").write_text("".join(console.errors))
            console.close()
        missing = create_fixture(sdk_root, directory / "missing-provider", artifacts, omit_plugin=True)
        evidence["missing_provider"] = qualify_missing_provider(missing, fixtures)
        evidence["scope"] = "signed_native_loader" if arguments.loader_only else "signed_native_loader_and_black_input_stream"
        evidence["native_host_input"] = False
        evidence["native_monitor_or_desktop_capture"] = False
        (directory / "evidence.json").write_text(json.dumps(evidence, indent=2) + "\n")
        print(json.dumps(evidence, sort_keys=True))
        print("Passed: isolated signed native composition. Real monitor, OS input and Moonlight playback remain unqualified.")
        succeeded = True
    finally:
        if succeeded and not arguments.keep:
            shutil.rmtree(directory)
        else:
            print(f"Private qualification evidence: {directory}", file=sys.stderr)


if __name__ == "__main__":
    main()
