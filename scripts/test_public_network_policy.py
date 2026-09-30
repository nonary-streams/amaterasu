#!/usr/bin/env python3
"""Signed Linux public listener policy, real UDP ownership and restart recovery.

Every listener uses loopback. Trusted RTSP tickets exercise transport ownership;
this does not qualify public launch, hardware capture or a Moonlight player.
"""
from __future__ import annotations

import hashlib
import http.client
import json
from pathlib import Path
import secrets
import socket
import ssl
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_management_api import Api, PAIRING_FIXTURES, stop_console
from test_management_origin_policy import issue, ready
from test_rtsp import RtspClient, request, ticket_config


def udp(host, port=0):
    stream = socket.socket(socket.AF_INET6 if ":" in host else socket.AF_INET, socket.SOCK_DGRAM)
    if ":" in host:
        stream.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    try:
        stream.bind((host, port))
        return stream
    except BaseException:
        stream.close()
        raise


def available_base(excluded=()):
    for _ in range(100):
        probes = []
        try:
            first = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            probes.append(first)
            first.bind(("127.0.0.1", 0))
            base = first.getsockname()[1]
            if not 1029 <= base <= 65514 or base in excluded:
                continue
            for host in ["127.0.0.1", "::1"]:
                for kind, offset in [(socket.SOCK_STREAM, 0), (socket.SOCK_STREAM, -5),
                                     (socket.SOCK_STREAM, 21), (socket.SOCK_DGRAM, 9),
                                     (socket.SOCK_DGRAM, 10), (socket.SOCK_DGRAM, 11)]:
                    if host == "127.0.0.1" and offset == 0:
                        continue
                    probe = socket.socket(socket.AF_INET6 if ":" in host else socket.AF_INET, kind)
                    probes.append(probe)
                    if ":" in host:
                        probe.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                    probe.bind((host, base + offset))
            return base
        except OSError:
            pass
        finally:
            for probe in probes:
                probe.close()
    raise AssertionError("no complete loopback IPv4/IPv6 public port set available")


def start_console(harness):
    console = Console(harness)
    deadline = time.monotonic() + 60
    try:
        while not any(line.startswith("Development worker ready: pid=") for line in console.errors):
            assert console.process.poll() is None, "".join(console.errors[-40:])
            assert time.monotonic() < deadline, "signed host startup deadline"
            time.sleep(0.025)
        ready(console)
        return console
    except BaseException:
        console.close()
        raise


def wait_tls(console):
    deadline = time.monotonic() + 20
    while not console.request("amaterasu.nvhttp/1", {"action": "status"})["tls_ready"]:
        assert time.monotonic() < deadline, "NVHTTP pinned TLS readiness"
        time.sleep(0.05)


def settings(api, token):
    status, result, _ = api.post("/v1/nvhttp/settings", token=token)
    assert status == 200 and result["ok"], (status, result)
    return result


def settled(api, token, base, hosts):
    deadline = time.monotonic() + 20
    while True:
        result = settings(api, token)
        expected = lambda offset: sorted(f"[{host}]:{base + offset}" if ":" in host
                                         else f"{host}:{base + offset}" for host in hosts)
        if (result["active_http_port"] == base and result["active_https_port"] == base - 5
                and result["active_rtsp_port"] == base
                and sorted(result["http_addresses"] or []) == expected(0)
                and sorted(result["https_addresses"] or []) == expected(-5)
                and sorted(result["rtsp_addresses"] or []) == expected(21)
                and result["listener_error"] is None and result["rtsp_listener_error"] is None):
            return result
        assert time.monotonic() < deadline, result
        time.sleep(0.05)


def update(api, token, previous, **values):
    status, result, _ = api.post("/v1/nvhttp/settings_update", {
        "expected_revision": previous["revision"], "hostname": previous["configured_hostname"],
        "port": previous["port"], **values,
    }, token)
    assert status == 200 and (result["ok"] or (
        result.get("code") == "listener_apply_pending" and result.get("persisted") is True
    )), (status, result)
    assert result["revision"] == previous["revision"] + 1, result
    readback = settings(api, token)
    assert readback["revision"] == result["revision"], (result, readback)
    return readback


def probe_tcp(base, hosts):
    context = ssl.create_default_context(cafile=str(PAIRING_FIXTURES / "host-cert.pem"))
    # This self-signed X.509 v1 fixture has only a CN and no SAN or certificate
    # extensions. Since the connected peers are IPs, hostname checks cannot
    # verify their identities. Keep CERT_REQUIRED and pin the exact DER; this
    # does not qualify endpoint identity or modern CA policy. Python 3.13+
    # enables strict CA checks by default.
    context.verify_flags &= ~ssl.VERIFY_X509_STRICT
    context.check_hostname = False
    certificate = ssl.PEM_cert_to_DER_cert((PAIRING_FIXTURES / "host-cert.pem").read_text())
    expected_certificate = hashlib.sha256(certificate).digest()
    for host in hosts:
        connection = http.client.HTTPConnection(host, base, timeout=3)
        try:
            connection.request("GET", "/serverinfo?uniqueid=network-policy")
            response = connection.getresponse()
            body = response.read()
            assert response.status == 200, (response.status, body)
            assert ET.fromstring(body).findtext("HttpsPort") == str(base - 5), body
        finally:
            connection.close()
        with socket.create_connection((host, base - 5), timeout=3) as transport:
            with context.wrap_socket(transport, server_hostname="fixture-host") as secure:
                assert hashlib.sha256(secure.getpeercert(binary_form=True)).digest() == expected_certificate
        with socket.create_connection((host, base + 21), timeout=3):
            pass


def open_ticket(console, host, name):
    video, audio = udp(host), udp(host)
    try:
        key = secrets.token_bytes(16)
        config = ticket_config(video, audio, key, name)
        config["peer_ip"] = host
        opened = request(console, config)
        assert opened["ok"] and opened["state"] == "ready", opened
        client = RtspClient(opened["local"], opened["rtsp_url"], key)
        # The older fixture parser retains brackets for IPv6 display strings.
        # socket.create_connection requires an unbracketed host tuple.
        client.address = (host, int(opened["local"].rsplit(":", 1)[1]))
        ports = client.setup()
        return opened, ports
    finally:
        video.close()
        audio.close()


def assert_udp_owned(host, base):
    for offset in [9, 10, 11]:
        try:
            unexpected = udp(host, base + offset)
        except OSError:
            continue
        unexpected.close()
        raise AssertionError(f"actual session did not own {host}:{base + offset}")


def close_ticket(console, ticket, host, base):
    closed = request(console, {"action": "close", "ticket": ticket})
    assert closed["ok"] and closed["state"] == "closed", closed
    for offset in [9, 10, 11]:
        udp(host, base + offset).close()


def occupied_after_preflight(console, host, base):
    blocker, video, audio = udp(host, base + 10), udp(host), udp(host)
    try:
        operation = "network-late-udp-conflict"
        config = ticket_config(video, audio, secrets.token_bytes(16), operation)
        config["peer_ip"] = host
        failed = request(console, config)
        assert failed["ok"] and failed["state"] == "failed", failed
        assert failed["failure"] == "prepare_rejected" and failed["cleanup_error"] is None, failed
        # Media allocated before control failed. Its two real sockets must be
        # released after confirmed session cleanup despite the later conflict.
        for offset in [9, 11]:
            udp(host, base + offset).close()
        lookup = request(console, {"action": "lookup", "operation_id": operation})
        assert lookup["state"] == "failed" and lookup["cleanup_error"] is None, lookup
    finally:
        blocker.close()
        video.close()
        audio.close()


def main():
    assert sys.platform.startswith("linux"), "this proof qualifies native Linux only"
    if "--skip-build" not in sys.argv[1:]:
        run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-public-network-") as temporary:
        harness = ComponentHarness(Path(temporary))
        base = available_base()
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        configured = bootstrap["policy"]["settings"]
        # StreamProbe's standalone pairing component intentionally has only
        # state RPC grants. This fixture imports its own pinned identity.
        configured.setdefault("amaterasu.pairing", {})["host_configuration"] = False
        configured["amaterasu.management"] = {"enabled": True, "bind": "127.0.0.1:0"}
        configured["amaterasu.nvhttp"] = {"enabled": True, "http_bind": f"127.0.0.1:{base}",
                                          "https_bind": f"127.0.0.1:{base - 5}"}
        configured["amaterasu.rtsp"] = {"enabled": True, "bind": f"127.0.0.1:{base + 21}",
                                       "allowed_principals": ["amaterasu.console", "amaterasu.nvhttp"]}
        configured["amaterasu.audio-encode"] = {"enabled": True}
        configured["amaterasu.stream-session"] = {"enabled": True, "allowed_principals": ["amaterasu.rtsp"]}
        for component in ["media-transport", "control-transport"]:
            configured[f"amaterasu.{component}"] = {"enabled": True, "allowed_principals": ["amaterasu.stream-session"]}
        bootstrap_path.write_text(json.dumps(bootstrap))
        package = harness.directory / "stream-probe"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = start_console(harness)
        try:
            console.sequence += 1
            console.send({"op": "request", "service": "amaterasu.pairing/1", "payload": {
                "action": "import_identity",
                "certificate_pem": (PAIRING_FIXTURES / "host-cert.pem").read_text(),
                "private_key_pem": (PAIRING_FIXTURES / "host-key.pem").read_text(),
            }, "timeout_ms": 10000, "operation_id": f"network.identity.{console.sequence}"})
            imported = console.receive()
            assert imported["kind"] == "result" and imported["payload"]["ok"], imported
            wait_tls(console)
            assert console.request("amaterasu.authentication/1", {"action": "setup", "username": "network-admin",
                                   "password": "isolated-network-policy-password"})["ok"]
            administrator = issue(console, ["configuration.read", "configuration.write"])
            reader = issue(console, ["configuration.read"])
            api = Api(console)
            current = settled(api, reader, base, ["127.0.0.1"])
            assert current["configured_address_family"] is None and current["configured_bind_address"] is None
            denied = api.post("/v1/nvhttp/settings_update", {"expected_revision": current["revision"],
                              "hostname": current["configured_hostname"], "port": current["port"],
                              "address_family": "both", "bind_address": ""}, reader)
            assert denied[0] == 403, denied
            current = update(api, administrator, current, address_family="both", bind_address="")
            current = settled(api, reader, base, ["127.0.0.1", "::1"])
            probe_tcp(base, ["127.0.0.1", "::1"])
            tickets = []
            for host in ["127.0.0.1", "::1"]:
                opened, ports = open_ticket(console, host, "network-" + host)
                assert {name: ports[name] for name in ["video", "control", "audio"]} == {
                    "video": base + 9, "control": base + 10, "audio": base + 11}, ports
                assert_udp_owned(host, base)
                tickets.append((opened["ticket"], host))
            print("Verified both TCP families and simultaneous actual IPv4/IPv6 UDP owners", flush=True)
            next_base = available_base([base])
            current = update(api, administrator, current, port=next_base)
            assert current["active_rtsp_port"] == base and current["rtsp_listener_error"], current
            for ticket, host in tickets:
                assert_udp_owned(host, base)
                close_ticket(console, ticket, host, base)
            current = settled(api, reader, next_base, ["127.0.0.1", "::1"])
            probe_tcp(next_base, ["127.0.0.1", "::1"])
            print("Verified active-ticket deferral and complete port move after release", flush=True)
            assert api.post("/v1/nvhttp/settings_update", {"expected_revision": current["revision"] - 1,
                            "hostname": current["configured_hostname"], "port": base},
                            administrator)[1]["code"] == "revision_conflict"
            blocked_base = available_base([base, next_base])
            blocker = udp("::1", blocked_base + 9)
            try:
                current = update(api, administrator, current, port=blocked_base)
                assert current["active_rtsp_port"] == next_base and current["rtsp_listener_error"], current
                assert sorted(current["rtsp_addresses"]) == sorted([
                    f"127.0.0.1:{next_base + 21}", f"[::1]:{next_base + 21}"]), current
                # Explicit old desired policy must settle again after failed apply.
                current = update(api, administrator, current, port=next_base)
                current = settled(api, reader, next_base, ["127.0.0.1", "::1"])
            finally:
                blocker.close()
            current = update(api, administrator, current, address_family="both", bind_address="::1")
            current = settled(api, reader, next_base, ["::1"])
            probe_tcp(next_base, ["::1"])
            current = update(api, administrator, current, hostname="network-policy")
            assert current["configured_address_family"] == "both" and current["configured_bind_address"] == "::1"
            revision = current["revision"]
            stop_console(console)
            console = start_console(harness)
            api = Api(console)
            current = settled(api, reader, next_base, ["::1"])
            assert current["revision"] == revision and current["configured_bind_address"] == "::1", current
            wait_tls(console)
            probe_tcp(next_base, ["::1"])
            occupied_after_preflight(console, "::1", next_base)
            opened, ports = open_ticket(console, "::1", "network-restart-v6")
            assert ports["video"] == next_base + 9 and ports["control"] == next_base + 10
            assert_udp_owned("::1", next_base)
            close_ticket(console, opened["ticket"], "::1", next_base)
            current = update(api, administrator, current, address_family=None, bind_address=None)
            current = settled(api, reader, next_base, ["127.0.0.1"])
            assert current["configured_address_family"] is None and current["configured_bind_address"] is None
            probe_tcp(next_base, ["127.0.0.1"])
            # Null resolves startup policy anew rather than persisting a resolved IP.
            revision = current["revision"]
            stop_console(console)
            configured["amaterasu.nvhttp"]["http_bind"] = f"[::1]:{base}"
            configured["amaterasu.nvhttp"]["https_bind"] = f"[::1]:{base - 5}"
            configured["amaterasu.rtsp"]["bind"] = f"[::1]:{base + 21}"
            bootstrap_path.write_text(json.dumps(bootstrap))
            console = start_console(harness)
            api = Api(console)
            current = settled(api, reader, next_base, ["::1"])
            assert current["revision"] == revision and current["configured_bind_address"] is None
            wait_tls(console)
            probe_tcp(next_base, ["::1"])
            console.eof()
        except BaseException:
            print("".join(console.errors[-40:]), file=sys.stderr)
            raise
        finally:
            console.close()
    print("Passed: signed Linux IPv4/IPv6 HTTP and pinned TLS listeners, encrypted RTSP, actual video/control/audio UDP ownership and release, active-ticket deferral, blocked UDP apply with old-policy recovery, later UDP conflict cleanup, durable CAS/omission/null and restart with changed startup policy. Remote peers, installed service, Windows/macOS and real client playback remain unqualified.")


if __name__ == "__main__":
    main()
