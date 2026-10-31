#!/usr/bin/env python3
"""Signed public software-policy forwarding and real unavailable-backend admission.

StreamProbe observes a bounded synthetic Video Open policy through real paired
HTTPS, RTSP and stream-session admission. Its video bytes are not decodable.
A separate PlatformHarness uses real default-feature video/encode libraries to
reject unavailable software before native allocation. Capture, native encoding,
hardware fallback, performance and client playback remain separate gates.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import secrets
import socket
import sys
import tempfile
import time
from urllib.parse import urlsplit

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_management_api import Api, ORIGIN, PASSWORD, available_moonlight_port_pair, stop_console
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_platform_modules import PlatformHarness, configure_steam_fixture
from test_public_launch import (
    SOURCE, capture_idle, child_running, launch_query,
    media_complete, other_paired_context, wait_until,
)
from test_rtsp import PeerDiscovery, RtspClient, announcement
from test_software_encoder_policy import checked, login
from test_stream_session import start_client, stop_client

FIELDS = ("sw_preset", "sw_tune", "encoder")
APPLICATION_NAME = "Controlled software policy process"


def wait_for_worker(console):
    deadline = time.monotonic() + 60
    while not any(line.startswith("Development worker ready: pid=") for line in console.errors):
        assert console.process.poll() is None, "signed host exited before worker readiness: " + "".join(console.errors)
        assert time.monotonic() < deadline, "signed host worker readiness timed out: " + "".join(console.errors)
        time.sleep(0.05)


def configure(harness, *, synthetic):
    configure_steam_fixture(harness)
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    settings = bootstrap["policy"]["settings"]
    settings["amaterasu.management"] = {
        "enabled": True, "bind": "127.0.0.1:0", "allowed_origins": [ORIGIN],
    }
    settings["amaterasu.pairing"]["host_configuration"] = False
    if synthetic:
        assert settings["amaterasu.catalog"]["export_principals"] == {
            "amaterasu.nvhttp": "amaterasu.management",
        }, settings["amaterasu.catalog"]
        base = available_moonlight_port_pair()
        settings["amaterasu.nvhttp"] = {
            "enabled": True, "http_bind": f"127.0.0.1:{base}",
            "https_bind": f"127.0.0.1:{base - 5}",
            "streaming": {
                "capture": {"capture_provider": "dxgi", "output": "synthetic-display",
                            "encoder": 0, "audio_endpoint": "default"},
                "profile": {
                    "max_width": 16, "max_height": 16, "max_fps": 30,
                    "bitrate_kbps": 1000, "max_reference_frames": 1,
                    "packet_size": 1024, "fec_percentage": 20,
                    "min_parity_shards": 2, "max_fec_blocks": 1,
                    "packet_ms": 10, "audio_bitrate": 128000,
                    "encrypt_video": True, "encrypt_audio": True,
                    "permit_legacy_audio_iv_wrap": False,
                },
            },
        }
        settings["amaterasu.rtsp"] = {
            "enabled": True, "bind": f"127.0.0.1:{base + 21}",
            "allowed_principals": ["amaterasu.nvhttp"],
        }
        settings["amaterasu.stream-session"] = {
            "enabled": True, "allowed_principals": ["amaterasu.rtsp"],
        }
        settings["amaterasu.audio-encode"] = {"enabled": True}
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {
                "enabled": True, "allowed_principals": ["amaterasu.stream-session"],
            }
    else:
        settings["amaterasu.encode"] = {"enabled": True}
        # The platform fixture grants DXGI, while Linux video defaults to
        # PipeWire/VKMS; select the provider this package actually grants.
        settings["amaterasu.video"] = {"enabled": True, "capture_providers": ["dxgi"]}
        settings["amaterasu.capture-dxgi"] = {"enabled": True}
    path.write_text(json.dumps(bootstrap))


def create_managed_application(api, bearer, marker):
    code = (
        "from pathlib import Path; import os,time; "
        f"Path({str(marker)!r}).write_text(str(os.getpid())); time.sleep(120)"
    )
    entry = {
        "name": APPLICATION_NAME,
        "launch": {
            "kind": "executable", "program": sys.executable,
            "arguments": ["-c", code], "auto_detach": False,
        },
    }
    for _ in range(5):
        catalog = checked(api, "/v1/catalog/list", bearer=bearer)
        status, created, _ = api.post("/v1/catalog/create", {
            "expected_revision": catalog["revision"], "entry": entry,
        }, bearer)
        assert status == 200, (status, created)
        if created.get("code") != "revision_conflict":
            break
    assert created.get("ok"), created
    app_id = created["id"]
    saved = checked(api, "/v1/catalog/get", {"id": app_id}, bearer)
    assert saved["id"] == app_id and saved["revision"] == created["revision"], saved
    assert saved["entry"]["name"] == APPLICATION_NAME, saved
    assert saved["entry"]["launch"]["program"] == sys.executable, saved
    assert saved["entry"]["launch"]["arguments"] == entry["launch"]["arguments"], saved
    assert saved["entry"]["launch"]["auto_detach"] is False, saved
    return app_id


def update_global_options(api, bearer, options):
    settings = checked(api, "/v1/nvhttp/settings", bearer=bearer)
    settings = checked(api, "/v1/nvhttp/settings_update", {
        "hostname": settings["configured_hostname"], "port": settings["port"],
        "expected_revision": settings["revision"], "lan_encryption_mode": 2,
        **dict.fromkeys(FIELDS), **options,
    }, bearer)
    assert {key: settings["configured_" + key] for key in FIELDS} == {
        **dict.fromkeys(FIELDS), **options,
    }, settings
    readback = checked(api, "/v1/nvhttp/settings", bearer=bearer)
    assert readback["revision"] == settings["revision"], readback
    assert {key: readback["configured_" + key] for key in FIELDS} == {
        **dict.fromkeys(FIELDS), **options,
    }, readback


def update_application_options(api, bearer, app_id, options):
    app = checked(api, "/v1/catalog/get", {"id": app_id}, bearer)
    assert app["entry"]["name"] == APPLICATION_NAME, app
    assert app["entry"]["launch"]["program"] == sys.executable, app
    updated = checked(api, "/v1/catalog/update", {
        "id": app_id, "expected_revision": app["revision"],
        "entry": {**app["entry"], "config_overrides": options},
    }, bearer)
    readback = checked(api, "/v1/catalog/get", {"id": app_id}, bearer)
    assert readback["id"] == app_id and readback["revision"] == updated["revision"], readback
    assert readback["entry"].get("config_overrides", {}) == {
        key: value for key, value in options.items() if value is not None
    }, readback
    assert readback["entry"]["launch"] == app["entry"]["launch"], readback


def update_client_options(api, bearer, certificate_id, options):
    clients = checked(api, "/v1/pairing/list_clients", bearer=bearer)
    updated = checked(api, "/v1/pairing/update_client_config_overrides", {
        "certificate_id": certificate_id, "expected_revision": clients["revision"],
        "config_overrides": options,
    }, bearer)
    assert updated["certificate_id"] == certificate_id, updated
    readback = checked(api, "/v1/pairing/list_clients", bearer=bearer)
    assert readback["revision"] == clients["revision"] + 1, readback
    by_certificate = {client["certificate_id"]: client for client in readback["clients"]}
    assert by_certificate[certificate_id].get("config_overrides", {}) == {
        key: value for key, value in options.items() if value is not None
    }, readback


def update_scopes(api, bearer, app_id, certificate_id, global_options,
                  app_options, client_options):
    update_global_options(api, bearer, global_options)
    update_application_options(api, bearer, app_id, app_options)
    if client_options is not None:
        update_client_options(api, bearer, certificate_id, client_options)


def project_stream(console, api, bearer, app_id, launch_id, https, context, foreign, certificate_id,
                   marker, oracle, directory, expected, mode):
    marker.unlink(missing_ok=True)
    baseline = console.request(SOURCE, {"action": "status"})
    key = secrets.token_bytes(16)
    query = launch_query(key, launch_id)
    query["uniqueid"] = "other-launch-owner"
    opened = successful(get(https, "/launch", query, context))
    url = opened.findtext("sessionUrl0")
    assert url and url.startswith("rtspenc://"), url
    wait_until(lambda: marker.exists() and marker.read_text().isdigit(), "application did not start")
    pid = int(marker.read_text())
    assert successful(get(https, "/launch", query, context)).findtext("sessionUrl0") == url
    assert int(marker.read_text()) == pid
    # Changes after launch admission cannot replace the issued ticket's snapshot.
    update_scopes(api, bearer, app_id, certificate_id,
                  {"sw_preset": "medium", "sw_tune": "psnr", "encoder": "software"},
                  {"sw_preset": "fast", "sw_tune": "ssim", "encoder": "auto"},
                  {"sw_preset": "  veryslow  ", "sw_tune": "", "encoder": "auto"})
    status, _ = get(https, "/cancel", context=foreign)
    assert status == 403 and child_running(pid), status
    # The changed client policy makes this a different request under an existing
    # key. Its rejection must leave the already issued encrypted ticket usable.
    status, replayed = get(https, "/launch", query, context)
    assert status == 409 and replayed.attrib["status_code"] == "409", (status, replayed.attrib)
    assert int(marker.read_text()) == pid
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio:
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        discovery = PeerDiscovery(video, audio)
        target = urlsplit(url)
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
            wait_until(lambda: media_complete(console, baseline), "synthetic stream did not start", 3)
            metrics = console.request(SOURCE, {"action": "status"})
            assert metrics["fixture"] is True, metrics
            assert metrics["video"]["last_open_policy"] == expected, metrics
            assert metrics["video"]["opened"] == baseline["video"]["opened"] + 1
            if mode == "management":
                snapshot = checked(api, "/v1/nvhttp/launch_status", bearer=bearer)["launch"]
                assert snapshot["state"] == "ready" and not snapshot["pending_resume"], snapshot
                assert len(snapshot["reference"]) == len(snapshot["revision"]) == 64, snapshot
                assert console.request("amaterasu.nvhttp/1", {"action":"management_launch_status"})["code"] == "permission_denied"
                reader = console.request("amaterasu.authentication/1", {
                    "action":"issue_token", "scopes":["session.read"], "ttl_seconds":3600,
                })["bearer"]
                payload = {"reference":snapshot["reference"], "revision":snapshot["revision"]}
                assert api.post("/v1/nvhttp/stop_launch", payload, reader)[0] == 403
                session_writer = console.request("amaterasu.authentication/1", {
                    "action":"issue_token", "scopes":["session.read", "session.write"], "ttl_seconds":3600,
                })["bearer"]
                assert api.post("/v1/nvhttp/stop_launch", payload, session_writer)[0] == 403
                wrong = {**payload, "revision":"0" * 64}
                status, rejected, _ = api.post("/v1/nvhttp/stop_launch", wrong, bearer)
                assert not rejected.get("ok") and rejected.get("code") == "launch_changed", (status, rejected)
                assert child_running(pid)
                accepted = checked(api, "/v1/nvhttp/stop_launch", payload, bearer)
                assert accepted["accepted"] and accepted["reference"] == snapshot["reference"], accepted
                def cleanup_confirmed():
                    result = checked(api, "/v1/nvhttp/launch_status", {"reference":snapshot["reference"]}, bearer)
                    assert result["launch"]["state"] != "cleanup_failed", result
                    return result["launch"]["state"] == "ended"
                wait_until(cleanup_confirmed, "management cleanup was not confirmed")
            elif mode == "teardown":
                client.transact("TEARDOWN", "/")
            else:
                successful(get(https, "/cancel", context=context))
            wait_until(lambda: not child_running(pid), "cleanup left application running")
            wait_until(lambda: capture_idle(console, baseline, opened_delta=1), "cleanup retained synthetic capture")
            stopped = console.request(SOURCE, {"action": "status"})
            assert stopped["video"]["last_open_policy"] == expected, stopped
            assert stopped["video"]["drains"] == baseline["video"]["drains"] + 1, stopped
            assert stopped["video"]["eos"] == baseline["video"]["eos"] + 1, stopped
        finally:
            if control is not None:
                stop_client(control)
            if "video" in discovery.destinations:
                discovery.close()
            else:
                discovery.decoy.close()


def exercise_public(directory, oracle):
    harness = ComponentHarness(directory)
    configure(harness, synthetic=True)
    package = directory / "signed-stream-probe"
    run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
    harness.stage(package)
    console = Console(harness)
    try:
        wait_for_worker(console)
        imported = console.request("amaterasu.pairing/1", {
            "action": "import_identity",
            "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
            "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
        })
        assert imported["ok"], imported
        state = wait_for(console, "amaterasu.nvhttp/1", {"action": "status"}, lambda value: value.get("tls_ready"))
        http = "http://" + state["http_address"]
        https = "https://" + state["https_address"]
        assert console.request("amaterasu.authentication/1", {
            "action": "setup", "username": "administrator", "password": PASSWORD,
        })["ok"]
        api = Api(console)
        bearer = login(api)
        certificate_id = pair(console, http)
        context = tls_context()
        foreign = other_paired_context(console, directory)
        marker = directory / "software-application.pid"
        app_id = create_managed_application(api, bearer, marker)
        exported = successful(get(https, "/applist", context=context))
        matches = [
            row for row in exported.findall("App")
            if row.findtext("AppTitle") == APPLICATION_NAME
        ]
        assert len(matches) == 1, [(row.findtext("ID"), row.findtext("AppTitle"))
                                   for row in exported.findall("App")]
        launch_id = matches[0].findtext("ID")
        assert launch_id and launch_id.isdecimal() and int(launch_id) > 0, launch_id
        cases = [
            ({}, {}, {}, {"sw_preset": "superfast", "sw_tune": "zerolatency", "encoder": 0}),
            ({"sw_preset": "fast", "sw_tune": "grain", "encoder": "auto"},
             {"sw_preset": "7", "sw_tune": "film,zerolatency"},
             {"sw_preset": "", "sw_tune": None, "encoder": "software"},
             {"sw_preset": "", "sw_tune": "film,zerolatency", "encoder": "software"}),
            ({"sw_preset": "  slow  ", "sw_tune": "grain", "encoder": "software"},
             {"sw_preset": None, "sw_tune": "  film,zerolatency  ", "encoder": "auto"},
             dict.fromkeys(FIELDS),
             {"sw_preset": "  slow  ", "sw_tune": "  film,zerolatency  ", "encoder": "auto"}),
            ({"sw_preset": "", "sw_tune": ""}, dict.fromkeys(FIELDS), dict.fromkeys(FIELDS),
             {"sw_preset": "", "sw_tune": "", "encoder": 0}),
        ]
        for index, (global_options, app_options, client_options, expected) in enumerate(cases):
            update_scopes(api, bearer, app_id, certificate_id,
                          global_options, app_options, client_options)
            project_stream(console, api, bearer, app_id, launch_id, https, context, foreign, certificate_id,
                           marker, oracle, directory, expected, "management" if index == 0 else "teardown" if index % 2 == 0 else "cancel")
        console.eof()
    except Exception:
        print("".join(console.errors))
        raise
    finally:
        stop_console(console)


def exercise_real_unavailable(directory):
    harness = PlatformHarness(directory)
    configure(harness, synthetic=False)
    harness.stage(harness.package(1))
    console = Console(harness)
    try:
        wait_for_worker(console)
        availability = console.request("amaterasu.encode/1", {"action": "enumerate"})
        assert availability["software_encoder"] == {
            "available": False, "reason": "x264_feature_disabled",
        }, availability
        request = {
            "action": "open", "operation_id": "software.unavailable",
            "capture_provider": "dxgi", "output": "unallocated-fixture-output",
            "encoder": "software", "packet_boundaries": True,
            "configuration": {"width": 16, "height": 16, "fps": 30, "bitrate": 1000000,
                              "sw_preset": "superfast", "sw_tune": "zerolatency"},
        }
        result = console.request("amaterasu.video/1", request)
        assert result == {"ok": False, "code": "x264_feature_disabled"}, result
        # no_resources is internal pipeline state, not a public response marker.
        # Resolver unit tests establish zero CaptureOpen effects. The signed run
        # checks the real unavailable result and settled operation below.
        lookup = console.request("amaterasu.video/1", {
            "action": "lookup", "operation_id": request["operation_id"],
        })
        assert lookup == {"ok": False, "code": "not_found"}, lookup
        replayed = console.request("amaterasu.video/1", request)
        assert replayed == {"ok": False, "code": "stream_closed"}, replayed
        status = console.request("amaterasu.video/1", {"action": "status"})
        assert status["ok"] is True and status["enabled"] is True, status
        assert len(status["streams"]) == 1, status
        failure = status["streams"][0]
        assert failure["state"] == "failed" and failure["error"] == "x264_feature_disabled", status
        assert failure["requested_encoder"] == "software", status
        console.eof()
    finally:
        stop_console(console)


def main():
    oracle = Path(os.environ["AMATERASU_ENET_CLIENT_ORACLE"]).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-software-projection-") as temporary:
        directory = Path(temporary)
        (directory / "public").mkdir()
        (directory / "unavailable").mkdir()
        exercise_public(directory / "public", oracle)
        exercise_real_unavailable(directory / "unavailable")
    print("Passed: signed paired HTTPS/RTSP/stream-session software policy requests, independent client/app/global inheritance, defaults/empty/raw values, frozen admission, owned cancel/drain/EOS and real unavailable-software rejection and operation cleanup.")
    print("Not qualified: native capture/encoding from the public synthetic fixture, hardware fallback, performance or client playback. Genuine software x264/decode tests are separate.")


if __name__ == "__main__":
    main()
