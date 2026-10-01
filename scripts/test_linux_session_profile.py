#!/usr/bin/env python3
"""Build, pack, stage and run the Linux session profile on loopback only.

This proves the composed host is reachable by a Moonlight-style wire client:
signed `pack-linux-session` package, staging into a private root, NVHTTP on
loopback ports, PIN pairing, a catalog entry created through the console, and
trust that survives a host restart. It starts no capture, display, audio or
input effect: the profile leaves them disabled, the host runs with an isolated
home directory and no session bus, display or sound-server variables, and the
test confirms that every socket the host owns is bound to loopback.

Cases that listen beyond loopback are separate and ignored by default, as in
`cargo test`. Run them with `--ignored` only where exposing the ports is
intended. Not covered here (pending): a LAN peer, a real Moonlight client, mDNS,
and any capture/display/audio/input effect.
"""
from __future__ import annotations

import argparse
import ipaddress
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import available_moonlight_port_pair
from test_nvhttp import (
    CLIENT_ID,
    FIXTURES,
    check_library,
    check_persisted_trust,
    get,
    pair,
    require_private_library,
    require_untrusted,
    successful,
    tls_context,
    wait_for,
)

# One cargo package per library the profile packs, plus the host and publisher.
# `pack-linux-session` names the first missing library if this list falls behind
# the profile in crates/release/src/profiles/linux_session.rs.
LIBRARY_PACKAGES = [
    "amaterasu-runtime", "amaterasu-state", "amaterasu-session", "amaterasu-configuration",
    "amaterasu-session-history", "amaterasu-host-metrics", "amaterasu-catalog",
    "amaterasu-application", "amaterasu-steam", "amaterasu-lutris", "amaterasu-authentication",
    "amaterasu-management", "amaterasu-pairing", "amaterasu-nvhttp", "amaterasu-display",
    "amaterasu-launch", "amaterasu-input", "amaterasu-virtual-display", "amaterasu-capture-kms",
    "amaterasu-audio", "amaterasu-audio-encode", "amaterasu-encode", "amaterasu-video",
    "amaterasu-media-transport", "amaterasu-control-transport", "amaterasu-frame-limiter",
    "amaterasu-stream-session", "amaterasu-rtsp",
]
# Software x264 loads at run time; the feature only compiles the binding in.
ENCODE_FEATURE = "amaterasu-encode/software-x264"
# Session-bus, display, and sound-server variables are removed so the host cannot
# reach the live desktop even if a module were enabled by mistake.
SESSION_ENVIRONMENT = (
    "DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "XDG_SESSION_ID",
    "XDG_SESSION_TYPE", "PULSE_SERVER", "PULSE_RUNTIME_PATH", "PIPEWIRE_RUNTIME_DIR",
    "XAUTHORITY", "KDE_FULL_SESSION", "WAYLAND_SOCKET",
)
HOST_READY_SECONDS = 20
CATALOG_NAME = 'Linux session & <loopback> "entry"'


def target_directory() -> Path:
    return Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target")).resolve()


def build() -> None:
    packages = ["amaterasu", "amaterasu-release", *LIBRARY_PACKAGES]
    command = [CARGO, "build", "--offline", "--locked", "--features", ENCODE_FEATURE]
    for package in packages:
        command += ["-p", package]
    result = subprocess.run(command, cwd=WORKSPACE, text=True, capture_output=True, timeout=3600)
    if result.returncode:
        raise AssertionError(f"build failed:\n{result.stdout}\n{result.stderr}")


def isolated_environment(directory: Path) -> dict[str, str]:
    home = directory / "home"
    runtime = home / "run"
    runtime.mkdir(parents=True, mode=0o700)
    environment = {key: value for key, value in os.environ.items() if key not in SESSION_ENVIRONMENT}
    environment.update(
        HOME=str(home),
        XDG_CONFIG_HOME=str(home / "config"),
        XDG_DATA_HOME=str(home / "data"),
        XDG_STATE_HOME=str(home / "state"),
        XDG_CACHE_HOME=str(home / "cache"),
        XDG_RUNTIME_DIR=str(runtime),
    )
    return environment


class LinuxSessionHarness:
    """Publisher and host for one private development root."""

    def __init__(self, directory: Path, init_arguments: list[str]) -> None:
        self.directory = directory
        self.root = directory / "host"
        self.artifacts = target_directory() / "debug"
        self.host = self.artifacts / "amaterasu"
        self.publisher = self.artifacts / "amaterasu-release"
        run(self.publisher, "init-linux-session", self.root, *init_arguments)
        self.environment = isolated_environment(directory)

    def pack_and_stage(self) -> None:
        package = self.directory / "linux-session-1"
        try:
            run(self.publisher, "pack-linux-session", self.root, self.artifacts, package, "1")
        except AssertionError as error:
            raise AssertionError(
                f"{error}\nIf a library is missing, add its package to LIBRARY_PACKAGES."
            ) from None
        run(self.publisher, "verify", self.root, package)
        run(self.publisher, "stage", self.root, package)

    def console(self) -> Console:
        return Console(self, environment=self.environment)


def rejected(*arguments: str | Path) -> str:
    """Run the publisher expecting a refusal; return its diagnostic."""
    result = subprocess.run(
        [str(argument) for argument in arguments],
        cwd=WORKSPACE, text=True, capture_output=True, timeout=60,
    )
    assert result.returncode != 0, f"unexpectedly accepted: {arguments}"
    return result.stderr


def check_rejected_network_options(publisher: Path, directory: Path) -> None:
    """Refusals happen before any file or socket exists."""
    cases = [
        (["--lan", "0.0.0.0:47989"], "--allow-remote"),
        (["--lan", "[::]:47989"], "--allow-remote"),
        (["--allow-remote"], "--allow-remote"),
        (["--lan", "192.168.1.20:47989", "--allow-remote"], "--allow-remote"),
        (["--lan", "127.0.0.1:1028"], "1029"),
        (["--lan", "127.0.0.1:65515"], "65514"),
        (["--lan", "224.0.0.251:47989"], "listener address"),
    ]
    for index, (arguments, expected) in enumerate(cases):
        root = directory / f"rejected-{index}"
        message = rejected(publisher, "init-linux-session", root, *arguments)
        assert expected in message, (arguments, message)
        assert not root.exists() or not (root / "bootstrap.json").exists(), arguments


def check_default_policy(root: Path) -> None:
    import json

    policy = json.loads((root / "bootstrap.json").read_text())["policy"]
    settings = policy["settings"]
    assert settings["amaterasu.linux-session-video"]["enabled"] is False
    assert settings["amaterasu.capture-kms"]["enabled"] is False
    assert settings["amaterasu.audio"]["enabled"] is False
    assert settings["amaterasu.virtual-display"]["enabled"] is False
    assert settings["amaterasu.input"]["allow_injection"] is False
    assert settings["amaterasu.display"]["allow_mutation"] is False
    assert settings["amaterasu.stream-session"]["enable_input"] is False
    assert settings["amaterasu.management"]["enabled"] is False
    for forbidden in ("input-probe", "software-probe-audio", "capture-dxgi", "capture-wgc",
                      "capture-pipewire", "capture-x11", "updater", "system-tray"):
        assert f"amaterasu.{forbidden}" not in policy["principals"], forbidden


# Linux /proc/net socket tables: (file, protocol, listening state code).
SOCKET_TABLES = (("tcp", "tcp", "0A"), ("tcp6", "tcp", "0A"), ("udp", "udp", "07"), ("udp6", "udp", "07"))


def parse_socket_address(text: str) -> tuple[ipaddress.IPv4Address | ipaddress.IPv6Address, int]:
    address, port = text.split(":")
    raw = bytes.fromhex(address)
    if len(raw) == 4:
        return ipaddress.IPv4Address(raw[::-1]), int(port, 16)
    words = b"".join(raw[index:index + 4][::-1] for index in range(0, 16, 4))
    return ipaddress.IPv6Address(words), int(port, 16)


def descendants(root_pid: int) -> set[int]:
    parents = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
            parents[int(entry.name)] = int(fields[1])
        except (OSError, IndexError, ValueError):
            continue
    found = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, parent in parents.items():
            if parent in found and pid not in found:
                found.add(pid)
                changed = True
    return found


def process_socket_inodes(pids: set[int]) -> set[str]:
    inodes = set()
    for pid in pids:
        try:
            for descriptor in Path(f"/proc/{pid}/fd").iterdir():
                target = os.readlink(descriptor)
                if target.startswith("socket:["):
                    inodes.add(target[8:-1])
        except OSError:
            continue
    return inodes


def host_listeners(host_pid: int) -> set[tuple[str, ipaddress.IPv4Address | ipaddress.IPv6Address, int]]:
    """Listening TCP and bound UDP sockets owned by the host and its workers."""
    inodes = process_socket_inodes(descendants(host_pid))
    listeners = set()
    for name, protocol, state in SOCKET_TABLES:
        for line in Path(f"/proc/net/{name}").read_text().splitlines()[1:]:
            fields = line.split()
            if fields[3] == state and fields[9] in inodes:
                address, port = parse_socket_address(fields[1])
                listeners.add((protocol, address, port))
    return listeners


def wait_for_listeners(host_pid: int, expected: set[tuple[str, int]]):
    deadline = time.monotonic() + HOST_READY_SECONDS
    while True:
        listeners = host_listeners(host_pid)
        if expected <= {(protocol, port) for protocol, _, port in listeners}:
            return listeners
        assert time.monotonic() < deadline, f"missing listeners {expected} in {listeners}"
        time.sleep(0.1)


def import_host_identity(console: Console) -> dict:
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
        "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
    })
    assert imported["ok"], imported
    return imported


def ready_status(console: Console) -> dict:
    return wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
                    lambda value: value.get("tls_ready"))


def pair_and_list(harness: LinuxSessionHarness, base_port: int) -> tuple[str, dict]:
    console = harness.console()
    try:
        imported = import_host_identity(console)
        status = ready_status(console)
        assert status["http_address"] == f"127.0.0.1:{base_port}", status
        assert status["https_address"] == f"127.0.0.1:{base_port - 5}", status
        http, https = "http://" + status["http_address"], "https://" + status["https_address"]
        information = successful(get(http, "/serverinfo", {"uniqueid": CLIENT_ID}))
        assert information.findtext("PairStatus") == "0"
        assert information.findtext("uniqueid") == imported["host_id"]

        created = console.request("amaterasu.catalog/1", {
            "action": "create", "expected_revision": 0,
            "entry": {"name": CATALOG_NAME, "launch": {
                "kind": "executable", "program": "/private/example-game",
            }},
        })
        assert created["ok"], created
        applications = {str(int(created["id"][4:], 16)): CATALOG_NAME}
        require_private_library(http, https)
        require_untrusted(https, tls_context())

        certificate_id = pair(console, http)
        context = tls_context()
        paired = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
        assert paired.findtext("PairStatus") == "1"
        check_library(https, applications, context)

        # Every socket the host and its workers own is loopback-only, and the
        # whole Moonlight port set is really listening.
        listeners = wait_for_listeners(console.process.pid, {
            ("tcp", base_port), ("tcp", base_port - 5), ("tcp", base_port + 21),
        })
        outside = {item for item in listeners if not item[1].is_loopback}
        assert not outside, f"listeners beyond loopback: {outside}"
        console.eof()
        return certificate_id, applications
    finally:
        console.close()


def confirm_restart_keeps_trust(harness: LinuxSessionHarness, certificate_id: str,
                                applications: dict) -> None:
    console = harness.console()
    try:
        status = ready_status(console)
        https = "https://" + status["https_address"]
        restored = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, tls_context()))
        assert restored.findtext("PairStatus") == "1", "pairing did not survive the restart"
        # Also proves the library persisted and that revocation still takes effect.
        check_persisted_trust(console, certificate_id, applications, revoke=True)
        console.eof()
    finally:
        console.close()


def test_loopback_pairing_catalog_and_restart() -> None:
    base_port = available_moonlight_port_pair()
    with tempfile.TemporaryDirectory(prefix="amaterasu-linux-session-") as temporary:
        directory = Path(temporary)
        harness = LinuxSessionHarness(
            directory, ["--lan", f"127.0.0.1:{base_port}", "--catalog-owner", "console"])
        check_default_policy(harness.root)
        check_rejected_network_options(harness.publisher, directory)
        harness.pack_and_stage()
        certificate_id, applications = pair_and_list(harness, base_port)
        confirm_restart_keeps_trust(harness, certificate_id, applications)
    print("Passed: signed pack-linux-session package, loopback NVHTTP/RTSP listeners, PIN pairing, "
          "console-authored application list, restart keeps PairStatus=1, and validated LAN "
          "option refusals. No capture, display, audio or input effect was started.")


IGNORED_LAN_REASON = (
    "binds wildcard 0.0.0.0 listeners on every interface of this machine; run it only "
    "where exposing the Moonlight ports is intended, and never as a firewall workaround"
)


def test_wildcard_lan_bind_opt_in() -> None:
    base_port = available_moonlight_port_pair()
    with tempfile.TemporaryDirectory(prefix="amaterasu-linux-session-lan-") as temporary:
        harness = LinuxSessionHarness(
            Path(temporary),
            ["--lan", f"0.0.0.0:{base_port}", "--allow-remote", "--catalog-owner", "console"])
        harness.pack_and_stage()
        console = harness.console()
        try:
            import_host_identity(console)
            status = ready_status(console)
            assert status["http_address"] == f"0.0.0.0:{base_port}", status
            http = f"http://127.0.0.1:{base_port}"
            successful(get(http, "/serverinfo", {"uniqueid": CLIENT_ID}))
            listeners = wait_for_listeners(console.process.pid, {
                ("tcp", base_port), ("tcp", base_port - 5), ("tcp", base_port + 21),
            })
            assert any(item[1].is_unspecified for item in listeners), listeners
            console.eof()
        finally:
            console.close()
    print("Passed: explicit --allow-remote wildcard listeners.")


DEFAULT_CASES = [test_loopback_pairing_catalog_and_restart]
IGNORED_CASES = [(test_wildcard_lan_bind_opt_in, IGNORED_LAN_REASON)]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--skip-build", action="store_true", help="reuse built artifacts")
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--ignored", action="store_true", help="run only the ignored cases")
    selection.add_argument("--include-ignored", action="store_true", help="run every case")
    arguments = parser.parse_args()
    if not arguments.skip_build:
        build()
    if not arguments.ignored:
        for case in DEFAULT_CASES:
            case()
    if arguments.ignored or arguments.include_ignored:
        for case, _ in IGNORED_CASES:
            case()
    else:
        for case, reason in IGNORED_CASES:
            print(f"ignored: {case.__name__}: {reason}")


if __name__ == "__main__":
    sys.exit(main())
