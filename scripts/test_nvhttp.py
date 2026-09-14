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


def get(base, path, query=None, context=None):
    url = base + path + ("?" + urlencode(query) if query else "")
    if context is not None:
        parsed = urlsplit(url)
        connection = HTTPSConnection(parsed.hostname, parsed.port, context=context, timeout=15)
        try:
            connection.connect()
            expected = x509.load_pem_x509_certificate((FIXTURES / "host-cert.pem").read_bytes())
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
        response = urlopen(url, context=context, timeout=15)
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


def tls_context(client="client", *, version=None):
    context = ssl.create_default_context(cafile=str(FIXTURES / "host-cert.pem"))
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


def pair(console, http):
    client_pem = (FIXTURES / "client-cert.pem").read_bytes()
    client_certificate = x509.load_pem_x509_certificate(client_pem)
    client_key = serialization.load_pem_private_key((FIXTURES / "client-key.pem").read_bytes(), None)
    salt = secrets.token_bytes(16)
    pin = "1234"
    key = hashlib.sha256(salt + pin.encode()).digest()[:16]
    begin = {
        "uniqueid": CLIENT_ID,
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
    expected = x509.load_pem_x509_certificate((FIXTURES / "host-cert.pem").read_bytes())
    assert server_certificate.fingerprint(hashes.SHA256()) == expected.fingerprint(hashes.SHA256())

    challenge = secrets.token_bytes(16)
    reply = successful(get(http, "/pair", {
        "uniqueid": CLIENT_ID, "clientchallenge": crypt(key, challenge).hex(),
    }))
    server_response = crypt(key, bytes.fromhex(reply.findtext("challengeresponse")), decrypt=True)
    assert len(server_response) == 48
    server_hash, server_challenge = server_response[:32], server_response[32:]
    client_secret = secrets.token_bytes(16)
    commitment = hashlib.sha256(server_challenge + client_certificate.signature + client_secret).digest()
    reply = successful(get(http, "/pair", {
        "uniqueid": CLIENT_ID, "serverchallengeresp": crypt(key, commitment).hex(),
    }))
    secret = bytes.fromhex(reply.findtext("pairingsecret"))
    server_secret, signature = secret[:16], secret[16:]
    server_certificate.public_key().verify(signature, server_secret, padding.PKCS1v15(), hashes.SHA256())
    assert server_hash == hashlib.sha256(challenge + server_certificate.signature + server_secret).digest()
    signed_secret = client_secret + client_key.sign(client_secret, padding.PKCS1v15(), hashes.SHA256())
    reply = successful(get(http, "/pair", {
        "uniqueid": CLIENT_ID, "clientpairingsecret": signed_secret.hex(),
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


def check_library(https, applications, context):
    root = successful(get(https, "/applist", {"uniqueid": CLIENT_ID}, context))
    rows = root.findall("App")
    assert len(rows) == len(applications), ET.tostring(root)
    assert {row.findtext("ID"): row.findtext("AppTitle") for row in rows} == applications
    assert all(row.findtext("IsHdrSupported") == "0" for row in rows)
    assert b"/private/example-game" not in ET.tostring(root), "launch metadata leaked"


def require_private_library(http, https):
    for base, context in ((http, None), (https, tls_context(None)), (https, tls_context("host"))):
        status, root = get(base, "/applist", {"uniqueid": CLIENT_ID}, context)
        assert status == 401 and not root.findall("App"), (status, ET.tostring(root))


def exercise(console):
    imported = console.request("amaterasu.pairing/1", {
        "action": "import_identity",
        "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
        "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
    })
    assert imported["ok"], imported
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
    status, rejected = get(http, "/unpair", {"uniqueid": CLIENT_ID})
    assert status != 200 and rejected.findtext("paired") != "1"
    return certificate_id, applications


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
    assert status != 200 and rejected.findtext("PairStatus") != "1", "revoked certificate retained access"
    status, rejected = get(https, "/applist", {"uniqueid": CLIENT_ID}, context)
    assert status == 401 and not rejected.findall("App"), "revoked certificate retained library access"


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
        console = Console(harness)
        try:
            certificate_id, applications = exercise(console)
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
    print("Passed: Gen7 pairing, pinned TLS 1.2/1.3, private paginated application list, stable IDs after rename/delete/restart, persisted trust and revocation. No streaming session opened.")


if __name__ == "__main__":
    main()
