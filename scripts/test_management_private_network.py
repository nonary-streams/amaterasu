#!/usr/bin/env python3
"""Opt-in signed HTTPS peer qualification in a disposable Linux network namespace.

The outer process never opens a remote listener. The child verifies separate
user/network namespaces and a loopback-only interface set before assigning test
addresses. No host interface, route, firewall rule or installed state is changed.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import http.client
import ipaddress
import json
import os
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import stop_console
from test_management_origin_policy import SERVICE, issue, ready
from test_platform_modules import PlatformHarness, configure_steam_fixture

SERVER_ADDRESS = "10.231.0.1"
PC_ADDRESS = "127.0.0.1"
LAN_ADDRESS = "10.231.0.2"
WAN_ADDRESS = "203.0.113.2"


def configure_private_network(parent_network, parent_user):
    assert os.readlink("/proc/self/ns/net") != parent_network, "network namespace was not isolated"
    assert os.readlink("/proc/self/ns/user") != parent_user, "user namespace was not isolated"
    assert socket.if_nameindex() == [(1, "lo")], "private network has unexpected interfaces"
    assert os.geteuid() == 0, "private namespace does not own its network configuration"
    tool = shutil.which("ip")
    assert tool is not None, "iproute2 is required for this opt-in test"
    subprocess.run([tool, "link", "set", "lo", "up"], check=True)
    for address in [SERVER_ADDRESS, LAN_ADDRESS, WAN_ADDRESS]:
        subprocess.run([tool, "address", "add", address + "/32", "dev", "lo"], check=True)


def provision_identity(harness):
    # A new private identity qualifies hostname and CA checks, without trusting
    # the repository fixture or disabling TLS certificate verification.
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Isolated management test")])
    now = datetime.now(timezone.utc)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=1))
        .not_valid_after(now + timedelta(hours=1))
        .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
        .add_extension(x509.SubjectAlternativeName([
            x509.IPAddress(ipaddress.ip_address(SERVER_ADDRESS)),
        ]), critical=False)
        .sign(key, hashes.SHA256())
    )
    material = {
        "cert": certificate.public_bytes(serialization.Encoding.PEM),
        "key": key.private_bytes(serialization.Encoding.PEM,
                                 serialization.PrivateFormat.PKCS8,
                                 serialization.NoEncryption()),
    }
    state_root = harness.root / "data/state"
    state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory = state_root / "management-tls"
    directory.mkdir(mode=0o700)
    paths = {}
    for role, contents in material.items():
        name = f"management-{role}-{hashlib.sha256(contents).hexdigest()}.pem"
        path = directory / name
        path.write_bytes(contents)
        path.chmod(0o600)
        paths[role] = path
    return paths


def configure_host(harness):
    configure_steam_fixture(harness)
    paths = provision_identity(harness)
    # Operational readiness also requires the real pairing identity provider.
    # These test-only Moonlight credentials are separate from browser TLS.
    fixtures = WORKSPACE / "modules/pairing/tests/fixtures"
    credentials = harness.root / "data/state/credentials"
    credentials.mkdir(mode=0o700)
    for target, source in [("cacert.pem", "host-cert.pem"), ("cakey.pem", "host-key.pem")]:
        path = credentials / target
        path.write_bytes((fixtures / source).read_bytes())
        path.chmod(0o600)
    with socket.socket() as reservation:
        reservation.bind((SERVER_ADDRESS, 0))
        port = reservation.getsockname()[1]
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.management"].update({
        "enabled": True, "bind": "127.0.0.1:0", "tls_bind": f"{SERVER_ADDRESS}:{port}",
        "tls_certificate_path": "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + paths["cert"].name,
        "tls_private_key_path": "__AMATERASU_WORKER_STATE_ROOT__/management-tls/" + paths["key"].name,
    })
    bootstrap["policy"]["settings"]["amaterasu.nvhttp"]["enabled"] = False
    path.write_text(json.dumps(bootstrap))
    return paths["cert"]


class PeerTlsApi:
    def __init__(self, console, certificate, source):
        status = console.request(SERVICE, {"action": "status"})
        assert status["ok"] and status["enabled"], status
        self.host, port = status["tls_address"].rsplit(":", 1)
        assert self.host == SERVER_ADDRESS, status
        self.port = int(port)
        self.source = source
        self.context = ssl.create_default_context(cafile=str(certificate))

    def connect(self, *, context=None, server_hostname=None):
        raw = socket.create_connection((self.host, self.port), timeout=4,
                                       source_address=(self.source, 0))
        try:
            assert raw.getsockname()[0] == self.source, raw.getsockname()
            return (context or self.context).wrap_socket(
                raw, server_hostname=server_hostname or self.host,
            )
        except BaseException:
            raw.close()
            raise

    def request(self, method, path, payload=None, token=None):
        connection = http.client.HTTPSConnection(
            self.host, self.port, timeout=4, context=self.context,
            source_address=(self.source, 0),
        )
        headers = {"Content-Type": "application/json"}
        if token is not None:
            headers["Authorization"] = "Bearer " + token
        try:
            connection.request(method, path, json.dumps(payload or {}) if method == "POST" else None, headers)
            assert connection.sock.getsockname()[0] == self.source
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def settings(self, token):
        status, result = self.request("POST", "/v1/management_settings/get", token=token)
        assert status == 200 and result["ok"], (status, result)
        return result


def assert_peer_denied(api):
    try:
        connection = api.connect()
    except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError, ssl.SSLEOFError):
        return
    connection.close()
    raise AssertionError(f"disallowed actual peer {api.source} completed its TLS handshake")


def assert_closed(connection):
    connection.settimeout(3)
    try:
        assert connection.recv(1) == b"", "narrowed peer retained an active connection"
    except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError, ssl.SSLEOFError):
        pass


def assert_identity_checks(api):
    for options in [
        {"context": ssl.create_default_context()},
        {"server_hostname": "wrong-identity.invalid"},
    ]:
        try:
            connection = api.connect(**options)
        except ssl.SSLCertVerificationError:
            continue
        connection.close()
        raise AssertionError("TLS accepted an untrusted certificate or mismatched identity")


def update_policy(api, token, selection):
    previous = api.settings(token)
    status, changed = api.request("POST", "/v1/management_settings/update", {
        "expected_revision": previous["revision"], "origin_web_ui_allowed": selection,
    }, token)
    assert status == 200 and changed["ok"], (status, changed)
    confirmed = api.settings(token)
    assert confirmed == changed and confirmed["revision"] == previous["revision"] + 1, (confirmed, changed)
    assert confirmed["values"] == {"origin_web_ui_allowed": selection}, confirmed
    return confirmed


def exercise_private_peers():
    with tempfile.TemporaryDirectory(prefix="amaterasu-private-peer-") as temporary:
        harness = PlatformHarness(Path(temporary))
        certificate = configure_host(harness)
        harness.stage(harness.package(1))
        console = Console(harness)
        try:
            initial = ready(console)
            assert initial["origin_web_ui_allowed"] == "lan" and initial["revision"] == 0, initial
            assert console.request("amaterasu.authentication/1", {
                "action": "setup", "username": "isolated-peer-test",
                "password": "isolated-peer-test-password",
            })["ok"]
            fixtures = WORKSPACE / "modules/pairing/tests/fixtures"
            identity = console.request("amaterasu.pairing/1", {
                "action": "import_identity",
                "certificate_pem": (fixtures / "host-cert.pem").read_text(),
                "private_key_pem": (fixtures / "host-key.pem").read_text(),
            })
            assert identity["ok"], identity
            administrator = issue(console, ["configuration.read", "configuration.write"])
            peers = {source: PeerTlsApi(console, certificate, source)
                     for source in [PC_ADDRESS, LAN_ADDRESS, WAN_ADDRESS]}
            pc, lan, wan = [peers[source] for source in [PC_ADDRESS, LAN_ADDRESS, WAN_ADDRESS]]
            assert_identity_checks(pc)
            assert pc.settings(administrator) == lan.settings(administrator)
            assert_peer_denied(wan)
            # The same actual LAN peer can read settings but not operational health.
            health = pc.request("GET", "/healthz")
            assert health == (200, {"ok": True, "authentication_verifier": True,
                                    "catalog_loaded": True, "pairing_identity": True}), health
            denied_health = lan.request("GET", "/healthz")
            assert denied_health == (403, {"ok": False, "code": "peer_denied"}), denied_health
            assert lan.request("POST", "/v1/management_settings/get")[0] == 401

            with lan.connect() as admitted:
                narrowed = update_policy(pc, administrator, "pc")
                assert_closed(admitted)
            assert_peer_denied(lan)
            assert_peer_denied(wan)
            stop_console(console)
            console = Console(harness)
            assert ready(console)["revision"] == narrowed["revision"]
            peers = {source: PeerTlsApi(console, certificate, source)
                     for source in [PC_ADDRESS, LAN_ADDRESS, WAN_ADDRESS]}
            pc, lan, wan = [peers[source] for source in [PC_ADDRESS, LAN_ADDRESS, WAN_ADDRESS]]
            assert pc.settings(administrator) == narrowed
            assert_peer_denied(lan)
            assert_peer_denied(wan)

            update_policy(pc, administrator, "wan")
            assert wan.settings(administrator) == lan.settings(administrator)
            assert wan.request("GET", "/healthz")[0] == 403
            with wan.connect() as admitted:
                reset = update_policy(pc, administrator, None)
                assert_closed(admitted)
            assert reset["origin_web_ui_allowed"] == "lan"
            assert lan.settings(administrator) == pc.settings(administrator)
            assert_peer_denied(wan)
        finally:
            console.close()
    print("Passed: signed native management HTTPS with verified private TLS identity, actual PC/LAN/WAN peers, authorization, narrowing disconnect, restart policy and PC-only health in an isolated Linux network namespace.")
    print("Unqualified: physical LAN/WAN reachability, installed TLS/service provisioning, browser editing, Windows/macOS, reference welcome exception and UPnP.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--private-network", action="store_true", required=True,
                        help="explicitly opt in to isolated non-loopback listener qualification")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--inside-private-network", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--parent-network", help=argparse.SUPPRESS)
    parser.add_argument("--parent-user", help=argparse.SUPPRESS)
    options = parser.parse_args()
    assert sys.platform.startswith("linux"), "this opt-in gate requires Linux"
    if options.inside_private_network:
        assert options.parent_network and options.parent_user
        configure_private_network(options.parent_network, options.parent_user)
        exercise_private_peers()
        return
    assert os.geteuid() != 0, "run this gate as the normal user, without sudo"
    if not options.skip_build:
        run(CARGO, "build", "--workspace", "--locked", "--offline")
    launcher = shutil.which("unshare")
    assert launcher is not None, "util-linux unshare is required for this opt-in gate"
    subprocess.run([
        launcher, "--user", "--map-root-user", "--net", sys.executable, str(Path(__file__).resolve()),
        "--private-network", "--skip-build", "--inside-private-network",
        "--parent-network", os.readlink("/proc/self/ns/net"),
        "--parent-user", os.readlink("/proc/self/ns/user"),
    ], check=True, timeout=180)


if __name__ == "__main__":
    main()
