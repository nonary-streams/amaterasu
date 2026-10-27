#!/usr/bin/env python3
"""Signed host integration on loopback with a private, verified TLS identity.

All persistent state and signing material live in a temporary root. No host
configuration, installed service, firewall rule, or non-loopback socket changes.
"""
from __future__ import annotations

import json
import argparse
import base64
import hashlib
import http.client
from pathlib import Path
import ssl
import shutil
import subprocess
import sys
import tempfile
import time

from test_component_updates import CARGO, WORKSPACE, run
from test_management_api import stop_console
import test_management_private_network as private
from test_platform_modules import PlatformHarness
from test_signed_browser_session import OwnedBrowser

PASSWORD = "integration-admin-password-1"
OTHER_PASSWORD = "integration-user-password-1"
NEW_PASSWORD = "integration-user-password-2"


def configure_extension(harness):
    source = harness.directory / "integration-extension-source"
    source.mkdir()
    example = WORKSPACE / "examples/web-extension"
    shutil.copy2(example / "example.js", source / "example.js")
    secret = harness.directory / "integration-extension-key"
    secret.write_text("42" * 32)
    secret.chmod(0o600)
    method = json.loads((example / "methods.json").read_text())[0]
    methods = harness.directory / "integration-extension-methods.json"
    methods.write_text(json.dumps([method]))
    binary = harness.artifacts / "amaterasu-web-extension"
    bundle = harness.directory / "integration-extension-bundle"
    output = run(binary, "pack", source, bundle, "hello", "Integration Fixture",
                 "example.js", "1", str(int(time.time()) + 3600), secret, methods)
    values = dict(line.split(": ", 1) for line in output.splitlines() if ": " in line)
    fingerprint = values["publisher fingerprint"]
    key = values["publisher public key"]
    root = harness.directory / "hello"
    run(binary, "stage", bundle, root, fingerprint, key)
    mismatched_root = harness.directory / "mismatched-policy" / "hello"
    shutil.copytree(root, mismatched_root)
    bootstrap_path = harness.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.management"]["extensions"] = {
        "entries": [
            {"id": "hello", "root": str(root), "trusted_keys": {fingerprint: key}, "methods": [method]},
            {"id": "different-id", "root": str(mismatched_root), "trusted_keys": {fingerprint: key}, "methods": [method]},
        ],
    }
    policy = bootstrap["policy"]
    auth = policy["settings"]["amaterasu.authentication"]
    if "account.self" not in auth["allowed_scopes"]:
        auth["allowed_scopes"].append("account.self")
        auth["authorization_policy_generation"] += 1
    policy["principals"]["example.extension-provider"] = {"provide": ["example.extension/1"]}
    policy["principals"]["amaterasu.management"].setdefault("call", []).append("example.extension/1")
    policy["api_methods"] = [{
        "provider": "example.extension-provider", "callers": ["amaterasu.management"],
        "method": {
            "service": "example.extension/1", "method": "greet", "interface_major": 1,
            "interface_minor": 0, "request": {"required": {"name": "string"}, "optional": {}, "allow_extra": False},
            "response": {"required": {"message": "string"}, "optional": {}, "allow_extra": False},
            "privilege": "user",
        },
    }]
    bootstrap_path.write_text(json.dumps(bootstrap))


def post(api, path, payload=None, bearer=None):
    status, body = api.request("POST", path, payload, bearer)
    # Assertions never include bodies: responses may contain bearer tokens.
    return status, body


def expect_success(result, label):
    status, body = result
    assert status == 200 and body.get("ok"), f"{label} failed (HTTP {status}, code {body.get('code')})"
    return body


def test_enrollment_and_tls(api, code):
    # Both trust and hostname verification must remain enabled.
    assert api.context.verify_mode == ssl.CERT_REQUIRED and api.context.check_hostname
    untrusted = ssl.create_default_context()
    try:
        api.connect(context=untrusted)
    except ssl.SSLCertVerificationError:
        pass
    else:
        raise AssertionError("untrusted TLS certificate was accepted")
    try:
        api.connect(server_hostname="wrong.invalid")
    except ssl.SSLCertVerificationError:
        pass
    else:
        raise AssertionError("TLS hostname mismatch was accepted")

    bad = post(api, "/v1/auth/enroll", {
        "code": "incorrect-enrollment-code", "username": "admin", "password": PASSWORD,
    })
    assert not bad[1].get("ok"), "invalid enrollment code was accepted"
    assert PASSWORD not in json.dumps(bad[1]) and code not in json.dumps(bad[1])
    expect_success(post(api, "/v1/auth/enroll", {
        "code": code, "username": "admin", "password": PASSWORD,
    }), "first enrollment")
    replay = post(api, "/v1/auth/enroll", {
        "code": code, "username": "admin2", "password": PASSWORD,
    })
    assert not replay[1].get("ok"), "enrollment code replay was accepted"
    login = expect_success(post(api, "/v1/auth/login", {
        "username": "admin", "password": PASSWORD,
    }), "administrator login")
    assert login.get("bearer"), "administrator login returned no bearer"
    return login


def expect_denied(result, label):
    status, body = result
    assert status in (401, 403) and not body.get("ok"), f"{label} unexpectedly allowed (HTTP {status})"


def login(api, username, password, role):
    result = expect_success(post(api, "/v1/auth/login", {
        "username": username, "password": password,
    }), f"{role} login")
    assert result.get("subject") == username and result.get("role") == role
    assert result.get("bearer") and result.get("refresh_token")
    return result


def test_accounts_and_sessions(api, administrator, harness, certificate):
    admin = administrator["bearer"]
    expect_denied(post(api, "/v1/accounts/list"), "anonymous account listing")
    for name, role in (("operator", "operator"), ("viewer", "viewer")):
        expect_success(post(api, "/v1/accounts/create", {
            "username": name, "password": OTHER_PASSWORD, "role": role,
        }, admin), f"create {role}")
    accounts = expect_success(post(api, "/v1/accounts/list", {}, admin), "account listing")
    assert {"admin", "operator", "viewer"} <= {account["username"] for account in accounts["accounts"]}
    operator = login(api, "operator", OTHER_PASSWORD, "operator")
    viewer = login(api, "viewer", OTHER_PASSWORD, "viewer")
    test_browser_cookies(api, harness, certificate)
    test_extension_visibility(api, administrator, operator, viewer)

    for session in (operator, viewer):
        token = session["bearer"]
        expect_success(post(api, "/v1/catalog/list", {}, token), "role catalog read")
        expect_denied(post(api, "/v1/accounts/list", {}, token), "non-admin account listing")
        expect_denied(post(api, "/v1/management_settings/update", {
            "expected_revision": 0, "origin_web_ui_allowed": "wan",
        }, token), "non-admin policy change")
    # Valid request bodies distinguish authorization failures from validation errors.
    missing_launch = {"operation_id": "integration.missing", "app_id": "app-0000000000000001"}
    expect_denied(post(api, "/v1/launch/start", missing_launch, viewer["bearer"]), "viewer launch")
    operator_launch = post(api, "/v1/launch/start", missing_launch, operator["bearer"])
    assert operator_launch[0] not in (401, 403), "operator launch rejected by authorization"

    rotated = expect_success(post(api, "/v1/auth/refresh", {
        "refresh_token": viewer["refresh_token"],
    }), "viewer refresh")
    assert rotated.get("role") == "viewer" and rotated.get("subject") == "viewer"
    assert rotated["bearer"] != viewer["bearer"] and rotated["refresh_token"] != viewer["refresh_token"]
    expect_denied(post(api, "/v1/catalog/list", {}, viewer["bearer"]), "rotated bearer")
    expect_denied(post(api, "/v1/auth/refresh", {"refresh_token": viewer["refresh_token"]}),
                  "refresh replay")
    expect_success(post(api, "/v1/catalog/list", {}, rotated["bearer"]), "rotated viewer read")

    # Updating one account must revoke its sessions, without disturbing another.
    expect_success(post(api, "/v1/accounts/update", {
        "username": "viewer", "role": "viewer", "enabled": False,
    }, admin), "disable viewer")
    expect_denied(post(api, "/v1/catalog/list", {}, rotated["bearer"]), "disabled bearer")
    expect_denied(post(api, "/v1/auth/refresh", {"refresh_token": rotated["refresh_token"]}),
                  "disabled refresh")
    expect_denied(post(api, "/v1/auth/login", {"username": "viewer", "password": OTHER_PASSWORD}),
                  "disabled login")
    expect_success(post(api, "/v1/catalog/list", {}, operator["bearer"]), "unaffected operator")
    expect_success(post(api, "/v1/accounts/list", {}, admin), "unaffected administrator after disable")

    expect_success(post(api, "/v1/accounts/set_password", {
        "username": "operator", "password": NEW_PASSWORD,
    }, admin), "reset operator password")
    expect_denied(post(api, "/v1/catalog/list", {}, operator["bearer"]), "reset bearer")
    expect_denied(post(api, "/v1/auth/refresh", {"refresh_token": operator["refresh_token"]}),
                  "reset refresh")
    expect_denied(post(api, "/v1/auth/login", {"username": "operator", "password": OTHER_PASSWORD}),
                  "old password")
    limited = post(api, "/v1/auth/login", {"username": "operator", "password": NEW_PASSWORD})
    assert limited[0] == 429 and limited[1].get("code") == "rate_limited", "login burst was not throttled"
    # Admission is deliberately capped per peer over a 60-second window.
    time.sleep(31)
    time.sleep(31)
    replacement = login(api, "operator", NEW_PASSWORD, "operator")
    expect_success(post(api, "/v1/auth/logout", {"refresh_token": replacement["refresh_token"]}), "logout")
    expect_denied(post(api, "/v1/catalog/list", {}, replacement["bearer"]), "logged-out bearer")
    expect_success(post(api, "/v1/accounts/delete", {"username": "viewer"}, admin), "delete viewer")
    expect_success(post(api, "/v1/catalog/list", {}, admin), "unaffected administrator")


def test_browser_cookies(api, harness, certificate):
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization

    chromium = shutil.which("chromium") or shutil.which("chromium-browser")
    assert chromium, "Chromium is required for the browser cookie integration"
    cert = x509.load_pem_x509_certificate(certificate.read_bytes())
    public = cert.public_key().public_bytes(serialization.Encoding.DER,
                                            serialization.PublicFormat.SubjectPublicKeyInfo)
    spki = base64.b64encode(hashlib.sha256(public).digest()).decode("ascii")
    browser = OwnedBrowser(harness.directory / "accounts-browser", chromium, spki)
    try:
        debugger = browser.start()
        result = subprocess.run(
            ["node", str(Path(__file__).with_name("network_accounts_browser_fixture.mjs"))],
            input=json.dumps({"debuggerUrl": debugger, "origin": f"https://{api.host}:{api.port}",
                              "adminPassword": PASSWORD, "viewerPassword": OTHER_PASSWORD}),
            capture_output=True, text=True, timeout=60,
        )
        assert result.returncode == 0, "browser cookie integration failed: " + result.stderr[-2000:]
    finally:
        browser.close()


def test_extension_visibility(api, administrator, operator, viewer):
    expect_denied(post(api, "/v1/extensions/list"), "anonymous extension listing")
    listings = []
    for session in (administrator, operator, viewer):
        status, listing = post(api, "/v1/extensions/list", {}, session["bearer"])
        assert status == 200, f"extension listing failed (HTTP {status})"
        assert len(listing["extensions"]) == 1
        listings.append(listing["extensions"][0])
    admin_extension, operator_extension, viewer_extension = listings
    assert admin_extension["id"] == "hello"
    assert admin_extension["digest"] == operator_extension["digest"] == viewer_extension["digest"]
    assert [item["alias"] for item in operator_extension["methods"]] == ["greet"]
    assert viewer_extension["methods"] == []
    call = {"id": "hello", "digest": admin_extension["digest"],
            "method": "greet", "payload": {"name": "integration"}}
    expect_denied(post(api, "/v1/extensions/call", call, viewer["bearer"]), "viewer extension call")
    status, reply = post(api, "/v1/extensions/call", call, operator["bearer"])
    assert status == 200 and reply.get("ok") and reply.get("result", {}).get("message") == "Hello, integration!", "authorized extension call failed"
    forged = dict(call, digest="0" * 64)
    status, reply = post(api, "/v1/extensions/call", forged, administrator["bearer"])
    assert status != 200 or not reply.get("ok"), "forged extension digest accepted"
    connection = http.client.HTTPSConnection(api.host, api.port, timeout=4, context=api.context,
                                             source_address=(api.source, 0))
    try:
        path = f"/v1/extensions/hello/{admin_extension['digest']}/assets/example.js"
        connection.request("GET", path, headers={"Authorization": "Bearer " + administrator["bearer"]})
        response = connection.getresponse()
        assert response.status == 200 and response.read() == (WORKSPACE / "examples/web-extension/example.js").read_bytes()
    finally:
        connection.close()


def test_host_lock(harness):
    issued = subprocess.run(
        [sys.executable, str(Path(__file__).with_name("enroll_linux_management.py")),
         str(harness.root), str(harness.host)], capture_output=True, text=True, timeout=60,
    )
    assert issued.returncode != 0 and "Enrollment code:" not in issued.stdout, \
        "enrollment provisioner bypassed the running host lock"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true", help="use an already-built workspace")
    options = parser.parse_args()
    if not options.skip_build:
        run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-network-accounts-") as temporary:
        harness = PlatformHarness(Path(temporary))
        # Reuse the private-peer fixture with a loopback-only server and client.
        private.SERVER_ADDRESS = "127.0.0.1"
        certificate = private.configure_host(harness)
        configure_extension(harness)
        package = harness.directory / "integration-platform"
        run(harness.publisher, "pack-extension-example", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        # The local operator command requires a stopped host and holds its lock.
        issued = subprocess.run(
            [sys.executable, str(Path(__file__).with_name("enroll_linux_management.py")),
             str(harness.root), str(harness.host)], capture_output=True, text=True, timeout=60,
        )
        assert issued.returncode == 0, "local enrollment provisioning failed"
        lines = issued.stdout.splitlines()
        codes = [line.removeprefix("Enrollment code: ") for line in lines
                 if line.startswith("Enrollment code: ")]
        assert len(codes) == 1 and codes[0], "local enrollment returned no code"
        console = private.start_ready_console(harness)
        try:
            private.ready(console)
            api = private.PeerTlsApi(console, certificate, "127.0.0.1")
            test_host_lock(harness)
            administrator = test_enrollment_and_tls(api, codes[0])
            test_accounts_and_sessions(api, administrator, harness, certificate)
        finally:
            stop_console(console)
    print("Passed: signed isolated host, verified TLS, enrollment and host lock; account roles, session isolation and throttling;")
    print("Chromium cookie login, CSRF, refresh and logout; signed extension selection, asset and authorized/denied calls.")


if __name__ == "__main__":
    main()
