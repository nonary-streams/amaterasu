#!/usr/bin/env python3
"""Pair over real HTTP, then prove TLS identity and live certificate revocation.

Uses deliberately public test identities and Python cryptography as an independent
protocol client. No display, capture, input, or streaming session is opened.
"""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.client import HTTPSConnection
import json
from pathlib import Path
import secrets
import socket
import ssl
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode, urlsplit
from urllib.request import urlopen
import xml.etree.ElementTree as ET

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_platform_modules import PlatformHarness

FIXTURES = WORKSPACE / "modules/pairing/tests/fixtures"
CLIENT_ID = "amaterasu-wire-test"


def get(base, path, query=None, context=None, timeout=15, *, expected_server_pem=None):
    url = base + path + ("?" + urlencode(query) if query else "")
    if context is not None:
        parsed = urlsplit(url)
        connection = HTTPSConnection(parsed.hostname, parsed.port, context=context, timeout=timeout)
        try:
            connection.connect()
            expected_pem = expected_server_pem or (FIXTURES / "host-cert.pem").read_bytes()
            expected = x509.load_pem_x509_certificate(expected_pem)
            assert secrets.compare_digest(
                connection.sock.getpeercert(binary_form=True),
                expected.public_bytes(serialization.Encoding.DER),
            ), "TLS peer differs from the pairing-owned identity"
            connection.request("GET", parsed.path + ("?" + parsed.query if parsed.query else ""))
            with connection.getresponse() as response:
                return decode_response(response)
        finally:
            connection.close()
    try:
        response = urlopen(url, context=context, timeout=timeout)
    except HTTPError as error:
        response = error
    with response:
        return decode_response(response)


def decode_response(response):
    body = response.read(65537)
    assert len(body) <= 65536, "unbounded wire reply"
    return response.status, ET.fromstring(body)


def successful(result):
    status, root = result
    assert status == 200 and root.get("status_code") == "200", (status, ET.tostring(root))
    return root


def wait_for(console, service, payload, predicate):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        result = console.request(service, payload)
        if predicate(result):
            return result
        time.sleep(0.025)
    raise AssertionError(f"Timed out waiting for {service}: {result}")


def crypt(key, data, *, decrypt=False):
    cipher = Cipher(algorithms.AES(key), modes.ECB())
    operation = cipher.decryptor() if decrypt else cipher.encryptor()
    return operation.update(data) + operation.finalize()


def tls_context(client="client", *, version=None, host_certificate_pem=None):
    if host_certificate_pem is None:
        context = ssl.create_default_context(cafile=str(FIXTURES / "host-cert.pem"))
    else:
        context = ssl.create_default_context(cadata=host_certificate_pem.decode("ascii"))
    context.check_hostname = False  # Fixture certificate has a test CN, not localhost.
    # The accepted legacy identity is X.509 v1, without modern CA extensions.
    # Keep certificate/signature verification and additionally pin its exact DER
    # before sending HTTP; disable only OpenSSL's stricter metadata requirements.
    context.verify_flags &= ~ssl.VERIFY_X509_STRICT
    if client:
        context.load_cert_chain(FIXTURES / f"{client}-cert.pem", FIXTURES / f"{client}-key.pem")
    if version:
        context.minimum_version = version
        context.maximum_version = version
    return context


def pair(console, http, *, host_certificate_pem=None, client_id=CLIENT_ID):
    client_pem = (FIXTURES / "client-cert.pem").read_bytes()
    client_certificate = x509.load_pem_x509_certificate(client_pem)
    client_key = serialization.load_pem_private_key((FIXTURES / "client-key.pem").read_bytes(), None)
    salt = secrets.token_bytes(16)
    pin = "1234"
    key = hashlib.sha256(salt + pin.encode()).digest()[:16]
    begin = {
        "uniqueid": client_id,
        "phrase": "getservercert",
        "salt": salt.hex(),
        "clientcert": client_pem.hex(),
        "devicename": "Independent Python wire client",
    }
    with ThreadPoolExecutor(max_workers=1) as executor:
        held = executor.submit(get, http, "/pair", begin)
        pending = wait_for(console, "amaterasu.pairing/1", {"action": "pending"},
                           lambda value: bool(value.get("pending")))
        assert not held.done(), "getservercert replied before PIN approval"
        pairing_id = pending["pending"][0]["pairing_id"]
        approved = console.request("amaterasu.pairing/1", {
            "action": "approve_pin", "pairing_id": pairing_id, "pin": pin,
        })
        assert approved["ok"], approved
        certificate_reply = successful(held.result(timeout=10))
    assert certificate_reply.findtext("paired") == "1"
    server_pem = bytes.fromhex(certificate_reply.findtext("plaincert"))
    server_certificate = x509.load_pem_x509_certificate(server_pem)
    expected_pem = host_certificate_pem or (FIXTURES / "host-cert.pem").read_bytes()
    expected = x509.load_pem_x509_certificate(expected_pem)
    assert server_certificate.fingerprint(hashes.SHA256()) == expected.fingerprint(hashes.SHA256())

    challenge = secrets.token_bytes(16)
    reply = successful(get(http, "/pair", {
        "uniqueid": client_id, "clientchallenge": crypt(key, challenge).hex(),
    }))
    server_response = crypt(key, bytes.fromhex(reply.findtext("challengeresponse")), decrypt=True)
    assert len(server_response) == 48
    server_hash, server_challenge = server_response[:32], server_response[32:]
    client_secret = secrets.token_bytes(16)
    commitment = hashlib.sha256(server_challenge + client_certificate.signature + client_secret).digest()
    reply = successful(get(http, "/pair", {
        "uniqueid": client_id, "serverchallengeresp": crypt(key, commitment).hex(),
    }))
    secret = bytes.fromhex(reply.findtext("pairingsecret"))
    server_secret, signature = secret[:16], secret[16:]
    server_certificate.public_key().verify(signature, server_secret, padding.PKCS1v15(), hashes.SHA256())
    assert server_hash == hashlib.sha256(challenge + server_certificate.signature + server_secret).digest()
    signed_secret = client_secret + client_key.sign(client_secret, padding.PKCS1v15(), hashes.SHA256())
    reply = successful(get(http, "/pair", {
        "uniqueid": client_id, "clientpairingsecret": signed_secret.hex(),
    }))
    assert reply.findtext("paired") == "1"
    return client_certificate.fingerprint(hashes.SHA256()).hex()


def require_untrusted(https, context):
    try:
        status, root = get(https, "/pair", {"uniqueid": CLIENT_ID, "phrase": "pairchallenge"}, context)
    except (URLError, ssl.SSLError, ConnectionError):
        return  # A TLS-layer rejection also denies access.
    assert status != 200 or root.get("status_code") != "200" or root.findtext("paired") != "1"


def create_library(console):
    """Exceed one catalog page, including characters that require XML escaping."""
    applications = {}
    for index in range(40):
        name = f'Game {index:02d} & <friends> "edition"'
        created = console.request("amaterasu.catalog/1", {
            "action": "create", "expected_revision": index,
            "entry": {"name": name, "launch": {
                "kind": "executable", "program": "/private/example-game",
            }},
        })
        assert created["ok"], created
        applications[str(int(created["id"][4:], 16))] = name
    return applications


def check_library(https, applications, context, *, client_id=CLIENT_ID, expected_server_pem=None):
    root = successful(get(
        https, "/applist", {"uniqueid": client_id}, context,
        expected_server_pem=expected_server_pem,
    ))
    rows = root.findall("App")
    assert len(rows) == len(applications), ET.tostring(root)
    assert {row.findtext("ID"): row.findtext("AppTitle") for row in rows} == applications
    assert all(row.findtext("IsHdrSupported") == "0" for row in rows)
    assert "<IsHdrSupported>1</IsHdrSupported>" not in ET.tostring(root, encoding="unicode")
    assert b"/private/example-game" not in ET.tostring(root), "launch metadata leaked"


def require_private_library(http, https):
    for base, context in ((http, None), (https, tls_context(None)), (https, tls_context("host"))):
        status, root = get(base, "/applist", {"uniqueid": CLIENT_ID}, context)
        assert status == 401 and not root.findall("App"), (status, ET.tostring(root))


def install_fixture_identity(harness):
    """Make the host-configured TLS identity the public pairing fixture.

    The development policy lets host configuration own the pairing identity. It
    generates credentials when none exist, so the fixture key pair is placed at
    the configured credential paths before the host first starts.
    """
    credentials = harness.root / "data" / "state" / "credentials"
    credentials.mkdir(parents=True, exist_ok=True)
    for source, name in (("host-cert.pem", "cacert.pem"), ("host-key.pem", "cakey.pem")):
        target = credentials / name
        target.write_bytes((FIXTURES / source).read_bytes())
        target.chmod(0o600)


def initialize_host_identity(console, *, timeout=90):
    """Wait for host configuration (which may generate a key) and return the identity."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            reply = console.request("amaterasu.pairing/1", {"action": "initialize"})
        except AssertionError:
            reply = None  # The request deadline elapsed while the key was being generated.
        if reply and reply.get("ok"):
            return reply
        assert time.monotonic() < deadline, f"host identity never became ready: {reply}"
        time.sleep(0.25)


def exercise(console):
    imported = initialize_host_identity(console)
    host_certificate = x509.load_pem_x509_certificate(imported["certificate_pem"].encode("ascii"))
    expected = x509.load_pem_x509_certificate((FIXTURES / "host-cert.pem").read_bytes())
    assert host_certificate.fingerprint(hashes.SHA256()) == expected.fingerprint(hashes.SHA256())
    status = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda value: value.get("tls_ready"))
    http, https = "http://" + status["http_address"], "https://" + status["https_address"]
    information = successful(get(http, "/serverinfo", {"uniqueid": CLIENT_ID}))
    assert information.findtext("PairStatus") == "0"
    assert information.findtext("uniqueid") == imported["host_id"]
    applications = create_library(console)
    require_private_library(http, https)
    require_untrusted(https, tls_context())
    certificate_id = pair(console, http)

    # Both supported TLS versions must use the exact pairing-owned host identity.
    for version in (ssl.TLSVersion.TLSv1_2, ssl.TLSVersion.TLSv1_3):
        context = tls_context(version=version)
        reply = successful(get(https, "/pair", {"uniqueid": CLIENT_ID, "phrase": "pairchallenge"}, context))
        assert reply.findtext("paired") == "1"
        info = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
        assert info.findtext("PairStatus") == "1"
        check_library(https, applications, context)
    renamed = console.request("amaterasu.catalog/1", {
        "action": "update", "id": "app-0000000000000001", "expected_revision": 40,
        "entry": {"name": "Renamed & retained ID", "launch": {
            "kind": "executable", "program": "/private/example-game",
        }},
    })
    assert renamed["ok"], renamed
    applications["1"] = "Renamed & retained ID"
    deleted = console.request("amaterasu.catalog/1", {
        "action": "delete", "id": "app-0000000000000002", "expected_revision": 41,
    })
    assert deleted["ok"], deleted
    del applications["2"]
    added = console.request("amaterasu.catalog/1", {
        "action": "create", "expected_revision": 42,
        "entry": {"name": "Added after deletion", "launch": {
            "kind": "executable", "program": "/private/example-game",
        }},
    })
    assert added["ok"] and added["id"] == "app-0000000000000029", added
    applications["41"] = "Added after deletion"
    check_library(https, applications, tls_context())
    require_untrusted(https, tls_context("host"))  # Valid TLS proof from an unpaired certificate.
    require_untrusted(https, tls_context(None))
    check_serverinfo_codec_contract(http, https)
    exercise_unpair(console, http, https)
    return certificate_id, applications


def assert_serverinfo_rejected(status, root, message):
    """An untrusted TLS /serverinfo is an HTTP success whose XML status is 401.

    moonlight-qt treats any non-2xx HTTP status as a network failure and only
    falls back to plain-HTTP /serverinfo (showing "not paired") for an XML 401.
    """
    assert status == 200 and root.get("status_code") == "401", (message, status, ET.tostring(root))
    assert root.findtext("PairStatus") is None, message


def check_serverinfo_codec_contract(http, https):
    """moonlight-common-c aborts LiStartConnection when ServerCodecModeSupport is 0."""
    for base, context in ((http, None), (https, tls_context())):
        info = successful(get(base, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
        mask = int(info.findtext("ServerCodecModeSupport"))
        assert mask & 0x1, f"H.264 bit missing from ServerCodecModeSupport={mask}"
        # Without a proven HEVC encoder no other codec, 10-bit, 4:4:4 or AV1 bit may appear.
        assert mask == 0x1 and info.findtext("MaxLumaPixelsHEVC") == "0", (mask, ET.tostring(info))


def exercise_unpair(console, http, https):
    """Plaintext /unpair is inert; paired TLS /unpair removes only the presented certificate."""
    context = tls_context()
    for query in ({"uniqueid": CLIENT_ID}, {"uniqueid": "someone-else", "uuid": "1"}, None):
        status, root = get(http, "/unpair", query)
        assert status == 200 and root.findtext("unpaired") == "0", (status, ET.tostring(root))
    still_paired = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
    assert still_paired.findtext("PairStatus") == "1", "plaintext /unpair changed trust"

    # The unpair reply must reach the client whose trust it removes.
    reply = successful(get(https, "/unpair", {"uniqueid": CLIENT_ID}, context))
    assert reply.findtext("unpaired") == "1", ET.tostring(reply)
    status, rejected = get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context)
    assert_serverinfo_rejected(status, rejected, "unpaired certificate kept access")
    status, rejected = get(https, "/applist", {"uniqueid": CLIENT_ID}, context)
    assert status == 401 and not rejected.findall("App"), "unpaired certificate kept library access"
    status, rejected = get(https, "/unpair", {"uniqueid": CLIENT_ID}, context)
    assert status == 401, "an already unpaired certificate must not authenticate"
    clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
    assert clients["ok"] and clients["clients"] == [], clients


def check_unpair_persisted_then_repair(console, applications, expected_certificate_id):
    """After a restart the self-unpaired client is still unpaired, and pairing works again."""
    status = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda value: value.get("tls_ready"))
    http, https = "http://" + status["http_address"], "https://" + status["https_address"]
    context = tls_context()
    clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
    assert clients["ok"] and clients["clients"] == [], "self-unpair did not survive restart"
    plaintext = successful(get(http, "/serverinfo", {"uniqueid": CLIENT_ID}))
    assert plaintext.findtext("PairStatus") == "0"
    status, rejected = get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context)
    assert_serverinfo_rejected(status, rejected, "unpaired certificate regained access")
    status, rejected = get(https, "/applist", {"uniqueid": CLIENT_ID}, context)
    assert status == 401 and not rejected.findall("App")

    certificate_id = pair(console, http)
    assert certificate_id == expected_certificate_id
    paired = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
    assert paired.findtext("PairStatus") == "1", "re-pairing after self-unpair failed"
    check_library(https, applications, context)
    return certificate_id


def check_persisted_trust(console, certificate_id, applications, *, revoke):
    status = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda value: value.get("tls_ready"))
    https = "https://" + status["https_address"]
    context = tls_context()
    if revoke:
        restored = successful(get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context))
        assert restored.findtext("PairStatus") == "1", "committed trust did not survive restart"
        check_library(https, applications, context)
        result = console.request("amaterasu.pairing/1", {"action": "unpair", "certificate_id": certificate_id})
        assert result["ok"], result
    status, rejected = get(https, "/serverinfo", {"uniqueid": CLIENT_ID}, context)
    assert_serverinfo_rejected(status, rejected, "revoked certificate retained access")
    status, rejected = get(https, "/applist", {"uniqueid": CLIENT_ID}, context)
    assert status == 401 and not rejected.findall("App"), "revoked certificate retained library access"


def available_loopback_moonlight_http_port():
    """Find a free loopback HTTP/HTTPS pair at Moonlight's conventional offset of 5."""
    for base in range(20_000, 45_000, 173):
        probes = []
        try:
            for port in (base, base - 5):
                probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                probes.append(probe)
                probe.bind(("127.0.0.1", port))
            return base
        except OSError:
            continue
        finally:
            for probe in probes:
                probe.close()
    raise AssertionError("no free loopback HTTP/HTTPS port pair")


def exercise_generated_identity_continuity():
    """A generated host identity, custom ports and a paired client survive host restart."""
    client_id = "generated-host-client"
    with tempfile.TemporaryDirectory(prefix="amaterasu-nvhttp-generated-") as temporary:
        harness = PlatformHarness(Path(temporary))
        http_port = available_loopback_moonlight_http_port()
        https_port = http_port - 5
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.nvhttp"] = {
            "enabled": True,
            "http_bind": f"127.0.0.1:{http_port}",
            "https_bind": f"127.0.0.1:{https_port}",
            "address_family": "ipv4",
            "bind_address": "127.0.0.1",
        }
        bootstrap["policy"]["settings"]["amaterasu.catalog"]["export_principals"] = {
            "amaterasu.nvhttp": "amaterasu.console",
        }
        path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))

        console = Console(harness)
        try:
            generated = initialize_host_identity(console)
            assert generated["ok"] and generated["host_id"], generated
            host_pem = generated["certificate_pem"].encode("ascii")
            assert "private_key" not in generated and "private_key_pem" not in generated
            status = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
                              lambda value: value.get("tls_ready"))
            assert status["http_address"].endswith(f":{http_port}"), status
            assert status["https_address"].endswith(f":{https_port}"), status
            http, https = "http://" + status["http_address"], "https://" + status["https_address"]
            info = successful(get(http, "/serverinfo", {"uniqueid": client_id}))
            assert info.findtext("uniqueid") == generated["host_id"]
            assert info.findtext("HttpsPort") == str(https_port)
            assert info.findtext("ExternalPort") == str(http_port)

            applications = create_library(console)
            certificate_id = pair(console, http, host_certificate_pem=host_pem, client_id=client_id)
            context = tls_context(host_certificate_pem=host_pem)
            secure_info = successful(get(
                https, "/serverinfo", {"uniqueid": client_id}, context,
                expected_server_pem=host_pem,
            ))
            assert secure_info.findtext("PairStatus") == "1"
            check_library(https, applications, context, client_id=client_id,
                          expected_server_pem=host_pem)
            console.eof()
        finally:
            console.close()

        # A fresh host process over the same isolated root keeps its public identity,
        # its ports and this client's pairing. The private key never leaves pairing.
        console = Console(harness)
        try:
            restored = console.request("amaterasu.pairing/1", {"action": "identity"})
            assert restored["ok"] and restored["host_id"] == generated["host_id"]
            assert restored["certificate_pem"] == generated["certificate_pem"]
            assert "private_key" not in restored and "private_key_pem" not in restored
            status = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"},
                              lambda value: value.get("tls_ready"))
            assert status["https_address"].endswith(f":{https_port}"), status
            https = "https://" + status["https_address"]
            context = tls_context(host_certificate_pem=host_pem)
            info = successful(get(
                https, "/serverinfo", {"uniqueid": client_id}, context,
                expected_server_pem=host_pem,
            ))
            assert info.findtext("uniqueid") == generated["host_id"]
            assert info.findtext("PairStatus") == "1"
            check_library(https, applications, context, client_id=client_id,
                          expected_server_pem=host_pem)
            clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
            assert any(item["certificate_id"] == certificate_id for item in clients["clients"])
            console.eof()
        finally:
            console.close()


def main():
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-nvhttp-") as temporary:
        harness = PlatformHarness(Path(temporary))
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.nvhttp"] = {
            "enabled": True, "http_bind": "127.0.0.1:0", "https_bind": "127.0.0.1:0",
        }
        # Production development policy exposes the management library. This
        # harness authors entries through its console, so bind that owner explicitly.
        bootstrap["policy"]["settings"]["amaterasu.catalog"]["export_principals"] = {
            "amaterasu.nvhttp": "amaterasu.console",
        }
        path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        install_fixture_identity(harness)
        console = Console(harness)
        try:
            certificate_id, applications = exercise(console)
            console.eof()
        finally:
            console.close()
        console = Console(harness)
        try:
            certificate_id = check_unpair_persisted_then_repair(console, applications, certificate_id)
            console.eof()
        finally:
            console.close()
        for revoke in (True, False):
            console = Console(harness)
            try:
                check_persisted_trust(console, certificate_id, applications, revoke=revoke)
                console.eof()
            finally:
                console.close()
    exercise_generated_identity_continuity()
    print("Passed: Gen7 pairing, pinned TLS 1.2/1.3, nonzero H.264 codec mask, inert plaintext and paired-TLS self-unpair with re-pairing after restart, private paginated application list, stable IDs after rename/delete/restart, persisted trust and revocation, generated-identity continuity on custom ports. No streaming session opened.")


if __name__ == "__main__":
    main()
