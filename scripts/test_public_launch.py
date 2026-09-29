#!/usr/bin/env python3
"""Paired HTTPS launch through a signed application/RTSP/media composition.

Uses a controlled child and synthetic video. This is protocol/lifecycle evidence,
not hardware capture, decoded H.264, display restoration or full Moonlight parity.
"""
from __future__ import annotations

import datetime
import json
import os
from pathlib import Path
import secrets
import socket
import sys
import tempfile
import time
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.x509.oid import NameOID

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_management_api import (
    APPLICATION_INPUT_OVERRIDES,
    CLIENT_INPUT_OVERRIDES,
    ORIGIN,
    PASSWORD,
    Api,
    available_moonlight_port_pair,
    assert_input_overrides,
)
from test_nvhttp import FIXTURES, get, pair, successful, tls_context, wait_for
from test_rtsp import PeerDiscovery, RtspClient, announcement
from test_stream_session import (
    collect,
    expected_video,
    start_client,
    stop_client,
    verify_audio,
    verify_video,
)

SOURCE = "test.stream-source/1"
INPUT_PROBE = "test.input-probe/1"


def launch_query(key, app_id="1", key_id=17):
    return {
        "appid": app_id,
        "mode": "16x16x30",
        "rikey": key.hex(),
        "rikeyid": str(key_id),
        "additionalStates": "1",
        "sops": "0",
        "localAudioPlayMode": "0",
        "surroundAudioInfo": "196610",
        "remoteControllersBitmap": "0",
        "gcmap": "0",
        "gcpersist": "0",
        "corever": "1",
    }


def child_running(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_until(predicate, message, seconds=5):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError(message)


def capture_idle(console, baseline, *, opened_delta=0):
    status = console.request(SOURCE, {"action": "status"})
    return all(
        status[medium]["active"] == 0
        and status[medium]["opened"] == baseline[medium]["opened"] + opened_delta
        and status[medium]["closed"] == baseline[medium]["closed"] + opened_delta
        for medium in ("video", "audio")
    )


def media_complete(console, baseline):
    status = console.request(SOURCE, {"action": "status"})
    return (
        status["video"]["frames"] == baseline["video"]["frames"] + 8
        and status["audio"]["frames"] == baseline["audio"]["frames"] + 8
        and status["video"]["force_idr"] > baseline["video"]["force_idr"]
    )


def create_application(console, marker, revision, lifetime, config_overrides=None):
    code = (
        f"from pathlib import Path; import os,time; "
        f"Path({str(marker)!r}).write_text(str(os.getpid())); time.sleep({lifetime!r})"
    )
    created = console.request(
        "amaterasu.catalog/1",
        {
            "action": "create",
            "expected_revision": revision,
            "entry": {
                "name": "Controlled streaming process",
                **(
                    {"config_overrides": config_overrides}
                    if config_overrides is not None
                    else {}
                ),
                "launch": {
                    "kind": "executable",
                    "program": sys.executable,
                    "arguments": ["-c", code],
                    # These controlled roots exercise ordinary attached-process
                    # exit and teardown, rather than the short-root detach rule.
                    "auto_detach": False,
                },
            },
        },
    )
    expected_id = f"app-{revision + 1:016x}"
    assert created["ok"] and created["id"] == expected_id, created
    return str(revision + 1)


def update_client_input_overrides(console, certificate_id, overrides):
    clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
    updated = console.request(
        "amaterasu.pairing/1",
        {
            "action": "update_client_config_overrides",
            "certificate_id": certificate_id,
            "expected_revision": clients["revision"],
            "config_overrides": overrides,
        },
    )
    assert updated["ok"], updated
    clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
    client = next(
        row for row in clients["clients"] if row["certificate_id"] == certificate_id
    )
    assert_input_overrides(client.get("config_overrides", {}), overrides)
    assert all(
        key not in client.get("config_overrides", {})
        for key, value in overrides.items()
        if value is None
    )


def update_application_input_overrides(console, app_id, overrides):
    app = console.request("amaterasu.catalog/1", {"action": "get", "id": app_id})
    updated = console.request(
        "amaterasu.catalog/1",
        {
            "action": "update",
            "id": app["id"],
            "expected_revision": app["revision"],
            "entry": {**app["entry"], "config_overrides": overrides},
        },
    )
    assert updated["ok"], updated
    app = console.request("amaterasu.catalog/1", {"action": "get", "id": app_id})
    assert_input_overrides(app["entry"].get("config_overrides", {}), overrides)
    assert all(
        key not in app["entry"].get("config_overrides", {})
        for key, value in overrides.items()
        if value is None
    )


def assert_persisted_input_overrides(console, certificate_id):
    app = console.request(
        "amaterasu.catalog/1", {"action": "get", "id": "app-0000000000000001"}
    )
    assert_input_overrides(app["entry"]["config_overrides"], APPLICATION_INPUT_OVERRIDES)
    clients = console.request("amaterasu.pairing/1", {"action": "list_clients"})
    client = next(
        row for row in clients["clients"] if row["certificate_id"] == certificate_id
    )
    assert_input_overrides(client["config_overrides"], CLIENT_INPUT_OVERRIDES)
    other = next(
        row for row in clients["clients"] if row["certificate_id"] != certificate_id
    )
    assert all(key not in other.get("config_overrides", {}) for key in CLIENT_INPUT_OVERRIDES)


def require_launch_denied(base, query, context=None, expected=401):
    try:
        status, _ = get(base, "/launch", query, context)
    except OSError:
        assert expected == 401
        return
    assert status == expected, status


def exercise_rejected_launches(console, http, https, paired, marker):
    marker.unlink(missing_ok=True)
    baseline = console.request(SOURCE, {"action": "status"})
    request = launch_query(secrets.token_bytes(16))
    require_launch_denied(http, request)
    require_launch_denied(https, request, tls_context(None))
    require_launch_denied(https, dict(request, owner="attacker"), paired, expected=400)
    time.sleep(0.1)
    assert not marker.exists(), "rejected launch started the catalog executable"
    assert capture_idle(console, baseline), "rejected launch changed capture state"


def exercise_natural_exit(console, https, context, marker, app_id, key_id):
    marker.unlink(missing_ok=True)
    baseline = console.request(SOURCE, {"action": "status"})
    query = launch_query(secrets.token_bytes(16), app_id, key_id)
    opened = successful(get(https, "/launch", query, context))
    assert opened.findtext("sessionUrl0", "").startswith("rtspenc://")
    wait_until(
        lambda: marker.exists() and marker.read_text().isdigit(),
        "short application did not start",
    )
    pid = int(marker.read_text())
    wait_until(lambda: not child_running(pid), "natural application exit was not reaped", 8)

    def exit_observed():
        server = successful(
            get(https, "/serverinfo", {"uniqueid": "public-launch"}, context)
        )
        return (
            server.findtext("state") == "AMATERASU_SERVER_FREE"
            and server.findtext("currentgame") == "0"
        )

    wait_until(exit_observed, "NVHTTP did not finish natural-exit cleanup", 8)
    cancelled = successful(get(https, "/cancel", context=context))
    assert cancelled.findtext("gamesession") == "0"
    assert capture_idle(console, baseline), "natural exit opened or retained capture"


def other_paired_context(console, directory):
    key = serialization.load_pem_private_key((FIXTURES / "client-key.pem").read_bytes(), None)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Other paired launch owner")])
    now = datetime.datetime.now(datetime.timezone.utc)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(name)
        .issuer_name(name)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=1))
        .sign(key, hashes.SHA256())
    )
    path = directory / "other-cert.pem"
    path.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
    imported = console.request(
        "amaterasu.pairing/1",
        {
            "action": "import_client",
            "client_id": "other-launch-owner",
            "name": "Other owner",
            "certificate_pem": path.read_text(),
        },
    )
    assert imported["ok"], imported
    context = tls_context(None)
    context.load_cert_chain(path, FIXTURES / "client-key.pem")
    return context


def exercise_stream(
    console,
    https,
    context,
    other,
    marker,
    oracle,
    directory,
    stop_mode,
    certificate_id,
    key_id,
    expected_input=None,
):
    marker.unlink(missing_ok=True)
    baseline = console.request(SOURCE, {"action": "status"})
    key = secrets.token_bytes(16)
    query = launch_query(key, key_id=key_id)
    # Payload identity cannot replace the authenticated certificate's policy.
    query["uniqueid"] = "other-launch-owner"
    opened = successful(get(https, "/launch", query, context))
    url = opened.findtext("sessionUrl0")
    assert url and url.startswith("rtspenc://"), url
    wait_until(
        lambda: marker.exists() and marker.read_text().isdigit(),
        "application did not start",
    )
    pid = int(marker.read_text())
    assert child_running(pid)
    retry = successful(get(https, "/launch", query, context))
    assert retry.findtext("sessionUrl0") == url, "launch retry replaced ticket"
    assert int(marker.read_text()) == pid, "launch retry spawned another process"
    foreign_status, foreign = get(https, "/cancel", context=other)
    assert foreign_status == 403, (foreign_status, foreign.attrib)
    assert foreign.get("status_message") == "stream is owned by another client"
    assert child_running(pid), "another paired certificate cancelled launch"
    after_foreign_cancel = successful(get(https, "/launch", query, context))
    assert after_foreign_cancel.findtext("sessionUrl0") == url
    assert int(marker.read_text()) == pid, "foreign cancel or retry replaced the process"

    with (
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as video,
        socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as audio,
    ):
        video.bind(("127.0.0.1", 0))
        audio.bind(("127.0.0.1", 0))
        association = PeerDiscovery(video, audio)
        target = urlsplit(url)
        client = RtspClient(f"{target.hostname}:{target.port}", url, key)
        ports = client.setup(association.on_setup)
        client.transact(
            "ANNOUNCE",
            "streamid=control/13/0",
            {"Content-Type": "application/sdp"},
            announcement(),
        )
        client.transact("PLAY", "/")
        control = start_client(
            oracle,
            directory,
            {"control_local": f"127.0.0.1:{ports['control']}"},
            key,
            ports["connect_data"],
        )
        try:
            association.finish(console, baseline)
            if expected_input is not None:
                wait_until(
                    lambda: console.request(INPUT_PROBE, {"action": "status"}).get("active"),
                    "stream did not acquire its synthetic input lease",
                )
                input_state = console.request(INPUT_PROBE, {"action": "status"})
                assert input_state["fixture"], input_state
                assert_input_overrides(input_state["requested_policy"], expected_input)
            wait_until(
                lambda: media_complete(console, baseline),
                "stream did not produce media and an IDR request",
                3,
            )
            if stop_mode == "teardown":
                client.transact("TEARDOWN", "/")
            elif stop_mode == "cancel":
                successful(get(https, "/cancel", context=context))
            else:
                revoked = console.request(
                    "amaterasu.pairing/1",
                    {"action": "unpair", "certificate_id": certificate_id},
                )
                assert revoked["ok"], revoked
            wait_until(
                lambda: not child_running(pid),
                "stream cleanup left its application running",
            )
            wait_until(
                lambda: capture_idle(console, baseline, opened_delta=1),
                "stream cleanup did not close each capture provider exactly once",
            )
            if expected_input is not None:
                wait_until(
                    lambda: not console.request(INPUT_PROBE, {"action": "status"})["active"],
                    "stream cleanup left its synthetic input lease active",
                )
            verify_video(collect(video, ("127.0.0.1", ports["video"])), key, expected_video)
            verify_audio(collect(audio, ("127.0.0.1", ports["audio"])), key, key_id)
        finally:
            stop_client(control)
            association.close()


def main():
    oracle = Path(os.environ["AMATERASU_ENET_CLIENT_ORACLE"]).resolve(strict=True)
    run(CARGO, "build", "--workspace", "--offline", "--locked")
    with tempfile.TemporaryDirectory(prefix="amaterasu-public-launch-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        path = harness.root / "bootstrap.json"
        bootstrap = json.loads(path.read_text())
        settings = bootstrap["policy"]["settings"]
        # Pairing uses an explicit fixture identity through its legacy importer.
        settings["amaterasu.pairing"]["host_configuration"] = False
        settings["amaterasu.management"] = {
            "enabled": True,
            "bind": "127.0.0.1:0",
            "allowed_origins": [ORIGIN],
        }
        settings["amaterasu.catalog"]["export_principals"] = {
            "amaterasu.nvhttp": "amaterasu.console"
        }
        # The settings API uses a Moonlight base port with derived listeners.
        # Choose a currently free loopback set instead of any installed host's
        # default ports, keeping that real protocol layout across restart.
        base_port = available_moonlight_port_pair()
        settings["amaterasu.nvhttp"] = {
            "enabled": True,
            "http_bind": f"127.0.0.1:{base_port}",
            "https_bind": f"127.0.0.1:{base_port - 5}",
            "streaming": {
                "capture": {
                    "capture_provider": "dxgi", "output": "synthetic-display",
                    "encoder": 0, "audio_endpoint": "default",
                },
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
            "enabled": True,
            "bind": f"127.0.0.1:{base_port + 21}",
            "allowed_principals": ["amaterasu.nvhttp"],
        }
        settings["amaterasu.stream-session"] = {
            "enabled": True,
            "enable_input": True,
            "allowed_principals": ["amaterasu.rtsp"],
        }
        settings["amaterasu.input-probe"] = {"enabled": True}
        settings["amaterasu.audio-encode"] = {"enabled": True}
        for component in ("media-transport", "control-transport"):
            settings[f"amaterasu.{component}"] = {
                "enabled": True,
                "allowed_principals": ["amaterasu.stream-session"],
            }
        path.write_text(json.dumps(bootstrap))
        package = directory / "package"
        run(harness.publisher, "pack-stream-probe", harness.root, harness.artifacts, package, "1")
        harness.stage(package)
        console = Console(harness)
        try:
            imported = console.request(
                "amaterasu.pairing/1",
                {
                    "action": "import_identity",
                    "certificate_pem": (FIXTURES / "host-cert.pem").read_text(),
                    "private_key_pem": (FIXTURES / "host-key.pem").read_text(),
                },
            )
            assert imported["ok"], imported
            state = wait_for(
                console,
                "amaterasu.nvhttp/1",
                {"action": "status"},
                lambda value: value.get("tls_ready"),
            )
            http, https = "http://" + state["http_address"], "https://" + state["https_address"]
            # This explicit fixture uses encrypted video on loopback. Its
            # durable LAN policy must agree with that client transport choice.
            api = Api(console)
            setup = console.request(
                "amaterasu.authentication/1",
                {"action": "setup", "username": "administrator", "password": PASSWORD},
            )
            assert setup["ok"], setup
            status, logged_in, _ = api.post(
                "/v1/auth/login",
                {"username": "administrator", "password": PASSWORD},
                origin=ORIGIN,
            )
            assert status == 200 and logged_in["ok"], logged_in
            administrator = logged_in["bearer"]
            status, host_settings, _ = api.post("/v1/nvhttp/settings", token=administrator)
            assert status == 200 and host_settings["ok"], host_settings
            status, encrypted, _ = api.post(
                "/v1/nvhttp/settings_update",
                {
                    "hostname": host_settings["hostname"],
                    "port": host_settings["port"],
                    "expected_revision": host_settings["revision"],
                    "lan_encryption_mode": 2,
                },
                administrator,
            )
            assert status == 200 and encrypted["ok"], encrypted
            assert encrypted["lan_encryption_mode"] == 2, encrypted
            certificate_id = pair(console, http)
            context = tls_context()
            other = other_paired_context(console, directory)
            marker = directory / "application.pid"
            short_marker = directory / "short-application.pid"
            create_application(console, marker, 0, 60, APPLICATION_INPUT_OVERRIDES)
            short_app = create_application(console, short_marker, 1, 3)
            update_client_input_overrides(console, certificate_id, CLIENT_INPUT_OVERRIDES)
            # Restart the signed host before observing the launch-time policy.
            console.eof()
            console.close()
            console = Console(harness)
            state = wait_for(
                console, "amaterasu.nvhttp/1", {"action": "status"},
                lambda value: value.get("tls_ready"),
            )
            http = "http://" + state["http_address"]
            https = "https://" + state["https_address"]
            api = Api(console)
            status, persisted_host, _ = api.post("/v1/nvhttp/settings", token=administrator)
            assert status == 200 and persisted_host["lan_encryption_mode"] == 2, persisted_host
            assert_persisted_input_overrides(console, certificate_id)
            exercise_rejected_launches(console, http, https, context, marker)
            exercise_natural_exit(console, https, context, short_marker, short_app, 17)
            # key_id is an IV salt, not session identity. Fresh AES keys may
            # legitimately reuse it, so every session deliberately uses 17.
            for mode in ("teardown", "cancel", "revoke"):
                if mode == "teardown":
                    expected_input = CLIENT_INPUT_OVERRIDES
                elif mode == "cancel":
                    update_client_input_overrides(
                        console, certificate_id,
                        {key: None for key in CLIENT_INPUT_OVERRIDES},
                    )
                    expected_input = APPLICATION_INPUT_OVERRIDES
                else:
                    update_application_input_overrides(
                        console, "app-0000000000000001",
                        {key: None for key in APPLICATION_INPUT_OVERRIDES},
                    )
                    expected_input = {key: None for key in APPLICATION_INPUT_OVERRIDES}
                exercise_stream(
                    console, https, context, other, marker, oracle, directory,
                    mode, certificate_id, 17, expected_input,
                )
            marker.unlink(missing_ok=True)
            require_launch_denied(https, launch_query(secrets.token_bytes(16)), context)
            assert not marker.exists(), "revoked certificate launched an application"
            console.eof()
        except Exception:
            print("".join(console.errors), file=sys.stderr)
            raise
        finally:
            console.close()
    print(
        "Passed: paired HTTPS exported application launch, owned retry/cancel, "
        "encrypted RTSP and discovered UDP media, TEARDOWN and "
        "certificate-revocation cleanup of capture and controlled child. "
        "Scoped input lease requests preserve certificate-bound client/application "
        "precedence, false/zero, null inheritance, and restart persistence. "
        "Hardware and full client parity remain unqualified."
    )


if __name__ == "__main__":
    main()
