#!/usr/bin/env python3
"""Signed software-policy CAS, authorization and restart with real native modules.

This uses the feature-disabled production codec and does not simulate encoding.
Native feature-enabled NV12/FFmpeg decode qualification is a separate test gate.
No capture, input lease, application process or public stream is opened here.
"""
from __future__ import annotations

import json
from pathlib import Path
import tempfile

from test_component_updates import CARGO, WORKSPACE, run
from test_host_service import Console
from test_management_api import Api, ORIGIN, PASSWORD, stop_console
from test_platform_modules import PlatformHarness, configure_steam_fixture

FIELDS = ("sw_preset", "sw_tune", "encoder", "min_threads")
FIXTURES = WORKSPACE / "modules/pairing/tests/fixtures"


def checked(api, path, payload=None, bearer=None):
    status, result, _ = api.post(path, payload, bearer)
    assert status == 200 and result.get("ok"), (path, status, result)
    return result


def login(api):
    return checked(api, "/v1/auth/login", {
        "username": "administrator", "password": PASSWORD,
    })["bearer"]


def selected(settings):
    return {field: settings["configured_" + field] for field in FIELDS}


def update_global(api, bearer, settings, **options):
    return checked(api, "/v1/nvhttp/settings_update", {
        "hostname": settings["configured_hostname"], "port": settings["port"],
        "expected_revision": settings["revision"], **options,
    }, bearer)


def issue(console, scopes):
    result = console.request("amaterasu.authentication/1", {
        "action": "issue_token", "scopes": scopes, "ttl_seconds": 3600,
    })
    assert result["ok"], result
    return result["bearer"]


def configure(harness):
    configure_steam_fixture(harness)
    # The platform pairing service uses real host-configuration identity paths.
    credentials = harness.root / "data" / "state" / "credentials"
    credentials.mkdir(parents=True, exist_ok=True)
    for target, fixture in (("cacert.pem", "host-cert.pem"), ("cakey.pem", "host-key.pem")):
        (credentials / target).write_bytes((FIXTURES / fixture).read_bytes())
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    bootstrap["policy"]["settings"]["amaterasu.management"] = {
        "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
    }
    bootstrap["policy"]["settings"]["amaterasu.encode"] = {"enabled": True}
    path.write_text(json.dumps(bootstrap))


def exercise(harness):
    console = Console(harness)
    try:
        api = Api(console)
        assert console.request("amaterasu.authentication/1", {
            "action": "setup", "username": "administrator", "password": PASSWORD,
        })["ok"]
        bearer = login(api)
        reader = issue(console, ["configuration.read"])
        forbidden = issue(console, ["catalog.read"])
        assert api.post("/v1/nvhttp/settings")[0] == 401
        assert api.post("/v1/nvhttp/settings", token=forbidden)[0] == 403
        settings = checked(api, "/v1/nvhttp/settings", bearer=reader)
        assert selected(settings) == dict.fromkeys(FIELDS), settings
        assert settings["effective_sw_preset"] == "superfast", settings
        assert settings["effective_sw_tune"] == "zerolatency", settings
        assert settings["effective_min_threads"] == 2, settings
        change = {"hostname": "Software policy", "port": settings["port"],
                  "expected_revision": settings["revision"], "sw_preset": "slow"}
        assert api.post("/v1/nvhttp/settings_update", change, reader)[0] == 403
        settings = update_global(api, bearer, settings,
                                 sw_preset="  slow  ", sw_tune="film,zerolatency", encoder="software",
                                 min_threads=8)
        assert selected(settings) == {"sw_preset": "  slow  ", "sw_tune": "film,zerolatency",
                                      "encoder": "software", "min_threads": 8}
        assert settings["effective_min_threads"] == 8, settings
        stale = dict(change, expected_revision=settings["revision"] - 1)
        status, conflict, _ = api.post("/v1/nvhttp/settings_update", stale, bearer)
        assert status == 200 and conflict.get("code") == "revision_conflict", conflict
        settings = update_global(api, bearer, settings, sw_tune="")
        assert settings["configured_sw_tune"] == "" and settings["effective_sw_tune"] == "", settings
        # An unrelated update omits all software fields and must retain them.
        settings = update_global(api, bearer, settings)
        assert selected(settings) == {"sw_preset": "  slow  ", "sw_tune": "", "encoder": "software",
                                      "min_threads": 8}
        for field, value in [("sw_preset", False), ("sw_tune", "x" * 257), ("encoder", "nvenc"),
                             ("min_threads", 0), ("min_threads", 257), ("min_threads", 1.5)]:
            invalid = {**change, "expected_revision": settings["revision"], field: value}
            assert api.post("/v1/nvhttp/settings_update", invalid, bearer)[0] == 400
        availability = checked(api, "/v1/media/encoders", bearer=bearer)
        assert availability["enabled"] is True, availability
        assert availability["software_encoder"] == {"available": False, "reason": "x264_feature_disabled"}, availability
        assert not any(entry["identity"]["clsid"] == "amaterasu.software.x264" for entry in availability["encoders"])

        catalog = checked(api, "/v1/catalog/list", bearer=bearer)
        app_options = {"sw_preset": "7", "sw_tune": "film,zerolatency", "encoder": "auto",
                       "min_threads": 7}
        entry = {"name": "Software policy scoped application", "config_overrides": app_options,
                 "launch": {"kind": "executable", "program": "/usr/bin/true"}}
        created = checked(api, "/v1/catalog/create", {"expected_revision": catalog["revision"], "entry": entry}, bearer)
        app = checked(api, "/v1/catalog/get", {"id": created["id"]}, bearer)
        assert app["entry"]["config_overrides"] == app_options, app
        assert api.post("/v1/catalog/update", {"id": created["id"], "expected_revision": created["revision"], "entry": entry}, reader)[0] == 403

        initialized = console.request("amaterasu.pairing/1", {
            "action": "import_identity",
            "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
            "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
        })
        assert initialized["ok"], initialized
        identities = []
        for certificate in ("client", "host"):
            imported = console.request("amaterasu.pairing/1", {
                "action": "import_client", "client_id": "same-claimed-client-id",
                "name": certificate, "certificate_pem": (FIXTURES / f"{certificate}-cert.pem").read_text(),
            })
            assert imported["ok"], imported
            identities.append(imported["certificate_id"])
        clients = checked(api, "/v1/pairing/list_clients", bearer=bearer)
        client_options = {"sw_preset": "", "sw_tune": None, "encoder": "software", "min_threads": 9}
        request = {"certificate_id": identities[0], "expected_revision": clients["revision"], "config_overrides": client_options}
        assert api.post("/v1/pairing/update_client_config_overrides", request, forbidden)[0] == 403
        checked(api, "/v1/pairing/update_client_config_overrides", request, bearer)
        updated_revision = checked(api, "/v1/pairing/list_clients", bearer=bearer)["revision"]
        assert updated_revision == clients["revision"] + 1
        status, conflict, _ = api.post("/v1/pairing/update_client_config_overrides", request, bearer)
        assert status == 200 and conflict.get("code") == "revision_conflict", conflict
        # Certificate IDs select persisted authority; claimed client IDs/PEM cannot override it.
        forged = {**request, "expected_revision": updated_revision, "client_id": "same-claimed-client-id"}
        assert api.post("/v1/pairing/update_client_config_overrides", forged, bearer)[0] == 400
        forged_pem = {**request, "expected_revision": updated_revision,
                      "certificate_pem": (FIXTURES / "host-cert.pem").read_text()}
        assert api.post("/v1/pairing/update_client_config_overrides", forged_pem, bearer)[0] == 400
        unknown = {**request, "expected_revision": updated_revision, "certificate_id": "0" * 64}
        status, refused, _ = api.post("/v1/pairing/update_client_config_overrides", unknown, bearer)
        assert status == 200 and refused.get("code") == "not_paired", refused
        clients = checked(api, "/v1/pairing/list_clients", bearer=bearer)
        by_certificate = {client["certificate_id"]: client for client in clients["clients"]}
        assert by_certificate[identities[0]]["config_overrides"] == {
            "sw_preset": "", "encoder": "software", "min_threads": 9,
        }, clients
        assert not by_certificate[identities[1]].get("config_overrides"), clients
        console.eof()
    finally:
        stop_console(console)

    console = Console(harness)
    try:
        api = Api(console)
        bearer = login(api)
        restored = checked(api, "/v1/nvhttp/settings", bearer=bearer)
        assert restored["revision"] == settings["revision"] and selected(restored) == selected(settings), restored
        app = checked(api, "/v1/catalog/get", {"id": created["id"]}, bearer)
        assert app["entry"]["config_overrides"] == app_options, app
        clients = checked(api, "/v1/pairing/list_clients", bearer=bearer)
        by_certificate = {client["certificate_id"]: client for client in clients["clients"]}
        assert by_certificate[identities[0]]["config_overrides"] == {
            "sw_preset": "", "encoder": "software", "min_threads": 9,
        }, clients
        assert not by_certificate[identities[1]].get("config_overrides"), clients
        reset = update_global(api, bearer, restored, sw_preset=None, sw_tune=None, encoder=None,
                              min_threads=None)
        assert selected(reset) == dict.fromkeys(FIELDS), reset
        assert (reset["effective_sw_preset"], reset["effective_sw_tune"]) == ("superfast", "zerolatency"), reset
        assert reset["effective_min_threads"] == 2, reset
        console.eof()
    finally:
        stop_console(console)


def main():
    run(CARGO, "build", "--workspace", "--locked", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-software-policy-") as temporary:
        harness = PlatformHarness(Path(temporary))
        configure(harness)
        harness.stage(harness.package(1))
        exercise(harness)
    print("Passed: signed real-module software policy global/app/certificate persistence, CAS, null/empty, authorization, restart and genuine disabled-feature availability.")
    print("Not qualified: public-stream consumer projection, native software-enabled encoding, platform capture or client playback; separate gates apply.")


if __name__ == "__main__":
    main()
