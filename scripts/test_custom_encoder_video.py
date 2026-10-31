#!/usr/bin/env python3
"""Signed source-encoder integration through the real video module.

The provider emits deliberately fake bytes; this checks routing and lifecycle,
not decoding or playback. All host state and signing keys are temporary.
"""
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_public_software_stream import CAPTURE, wait_ready, wait_until

CONTROL = "example.encode/1"
NATIVE = "example.encode-provider/1"
CONSUMER = "example.encode-consumer/1"
CUSTOM_CAPTURE = "test.custom-capture/1"
VIDEO = "amaterasu.video/1"
IDENTITY = {"clsid": "example.fake-h264", "hardware_url": None, "friendly_name": "Example fake H.264"}
CONFIGURATION = {"width": 64, "height": 48, "fps": 30, "bitrate": 1000000}


def configure(harness):
    path = harness.root / "bootstrap.json"
    bootstrap = json.loads(path.read_text())
    policy = bootstrap["policy"]
    policy["settings"]["amaterasu.video"] = {
        "enabled": True, "capture_providers": [],
        "source_encoder_providers": {"fixture": {
            "control_service": CONTROL, "native_service": NATIVE, "source_ids": [42],
        }},
        "external_capture_providers": {"custom": {"service": CUSTOM_CAPTURE, "frame_transport": "generic_cpu"}},
        "external_encoder_providers": {"generic": CONTROL},
        "external_encoder_consumers": {"generic": CONSUMER},
    }
    policy["settings"]["amaterasu.capture-source"] = {"custom_service": True}
    policy["settings"]["amaterasu.software-probe-audio"] = {"enable_video": False}
    policy["settings"]["example.encoder-source"] = {}
    policy["principals"]["example.encoder-source"] = {
        "provide": [CONTROL, NATIVE, CONSUMER], "native_provide": [CONTROL, NATIVE, CONSUMER],
    }
    policy["principals"]["amaterasu.video"]["call"].append(CONTROL)
    policy["principals"]["amaterasu.video"]["native_use"].append(NATIVE)
    policy["principals"]["amaterasu.video"]["native_use"].extend([CONTROL, CONSUMER, CUSTOM_CAPTURE])
    policy["principals"]["amaterasu.video"]["call"].append(CUSTOM_CAPTURE)
    policy["principals"]["amaterasu.console"]["call"].append(CONTROL)
    policy["principals"]["amaterasu.media-probe"]["call"].append(CONTROL)
    policy["principals"]["amaterasu.media-probe"]["call"].extend([CUSTOM_CAPTURE, CAPTURE])
    path.write_text(json.dumps(bootstrap))

    package = harness.directory / "encoder-package"
    run(harness.publisher, "pack-software-probe", harness.root, harness.artifacts, package, "1")
    manifest_path = package / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    components = {entry["id"]: entry for entry in manifest["components"]}
    video = components["amaterasu.video"]
    video["requested"]["call"].append(CONTROL)
    video["requested"]["native_use"].append(NATIVE)
    video["requested"]["native_use"].extend([CONTROL, CONSUMER])
    video["requires"]["example.encoder-source"] = "0.1.0"
    components["amaterasu.media-probe"]["requested"]["call"].append(CONTROL)
    components["amaterasu.media-probe"]["requested"]["call"].append(CAPTURE)
    provider = copy.deepcopy(components["amaterasu.capture-source"])
    library_name = ("amaterasu_encoder_source.dll" if os.name == "nt" else
                    "libamaterasu_encoder_source.dylib" if sys.platform == "darwin" else
                    "libamaterasu_encoder_source.so")
    library = harness.artifacts / library_name
    shutil.copy2(library, package / library.name)
    provider.update({"id": "example.encoder-source", "file": library.name,
                     "size": library.stat().st_size,
                     "sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
                     "requires": {"amaterasu.runtime": "0.1.0"}})
    provider["requested"] = {"provide": [CONTROL, NATIVE, CONSUMER], "call": [], "publish": [],
                             "native_provide": [CONTROL, NATIVE, CONSUMER], "native_use": []}
    manifest["components"].append(provider)
    data = json.dumps(manifest, indent=2).encode()
    manifest_path.write_bytes(data)
    secret = bytes.fromhex(json.loads((harness.root / "development-key.json").read_text()))
    signature = Ed25519PrivateKey.from_private_bytes(secret).sign(data).hex()
    (package / "manifest.sig").write_text(json.dumps({"key_id": "local-development", "signature": signature}))
    run(harness.publisher, "verify", harness.root, package)
    harness.stage(package)


def open_video(operation, **overrides):
    request = {"action": "open", "operation_id": operation, "source_id": 42,
               "encoder_provider": "fixture", "packet_boundaries": True,
               "encoder": IDENTITY, "configuration": CONFIGURATION}
    return {**request, **overrides}


def main():
    target = Path(os.environ["CARGO_TARGET_DIR"])
    assert target.is_absolute(), "use an isolated absolute Cargo target"
    run(CARGO, "build", "--workspace", "--locked", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-custom-encoder-") as temporary:
        harness = ComponentHarness(Path(temporary))
        configure(harness)
        console = Console(harness)
        try:
            wait_ready(console)
            assert console.request(CAPTURE, {"action": "status"})["opened"] == 0
            listed = console.request(CONTROL, {"action": "enumerate"})
            assert listed["ok"] and listed["encoders"][0]["identity"] == IDENTITY, listed
            request = {"action": "open", "operation_id": "direct-source", "mode": "source", "source_id": 42,
                       "encoder": IDENTITY, "configuration": CONFIGURATION, "packet_boundaries": True}
            direct = console.request(CONTROL, request)
            assert direct["ok"] and direct["packet_boundaries"], direct
            assert console.request(CONTROL, {"action": "lookup", "operation_id": "direct-source"})["session_id"] == direct["session_id"]
            assert console.request(CONTROL, {"action": "close", "session_id": direct["session_id"]})["ok"]
            assert console.request(CONTROL, {"action": "lookup", "operation_id": "direct-source"})["code"] == "session_not_found"
            contract = {"action": "open", "operation_id": "direct-generic", "encoder": IDENTITY,
                        "configuration": CONFIGURATION, "resource_output_clock": {
                            "domain": "stream_relative", "epoch_id": "01" * 16,
                            "host_origin_us": time.monotonic_ns() // 1000,
                        }}
            for value in (None, "unsupported"):
                candidate = dict(contract)
                if value is not None:
                    candidate["resource_callback_contract"] = value
                denied = console.request(CONTROL, candidate)
                assert denied["code"] == "resource_callback_contract_unsupported", denied
            contract["resource_callback_contract"] = "cross_service_safe"
            agreed = console.request(CONTROL, contract)
            assert agreed["ok"] and agreed["resource_callback_contract"] == "cross_service_safe", agreed
            assert agreed["resource_output_clock"] == contract["resource_output_clock"], agreed
            assert agreed["resource_sequence_policy"] == "strict_source_echo", agreed
            assert console.request(CONTROL, {"action": "close", "session_id": agreed["session_id"]})["ok"]
            for options in ({"source_id": 43}, {"encoder_provider": "unlisted"}):
                denied = console.request(VIDEO, open_video("denied-" + next(iter(options)), **options))
                assert not denied["ok"], denied
            own = console.request(VIDEO, open_video("console-owned"))
            assert own["ok"], own
            foreign = console.request("test.media-probe/1", {"action": "source_video_foreign_receive", "stream": own["stream"]})
            assert foreign["status"] == 12, foreign  # MEDIA_CLOSED for a foreign owner
            foreign_close = console.request("test.media-probe/1", {"action": "source_video_foreign_close", "stream": own["stream"]})
            assert foreign_close == {"ok": False, "code": "not_found"}, foreign_close
            assert console.request(VIDEO, {"action": "status", "stream": own["stream"]})["streams"], own
            assert console.request(VIDEO, {"action": "close", "stream": own["stream"]})["ok"]
            before_us = time.monotonic_ns() // 1000
            console.send({"op": "request", "service": "test.media-probe/1", "payload": {"action": "source_video"},
                          "timeout_ms": 10000, "operation_id": "source-video-test"})
            response = console.receive()
            after_us = time.monotonic_ns() // 1000
            assert response["kind"] == "result", response
            result = response["payload"]
            assert result["ok"], result
            packets = result["packets"]
            assert len(packets) == 2, packets
            unit, eos = packets
            origin = eos.pop("timestamp_us")
            timestamp = unit.pop("timestamp_us")
            assert before_us <= origin <= after_us and timestamp == origin + 1000, packets
            assert unit == {"codec": 1, "flags": 5, "sequence": 1,
                            "bytes_hex": b"FAKE-SOURCE-AU\0".hex()}, unit
            assert eos == {"codec": 1, "flags": 2, "sequence": 0, "bytes_hex": ""}, eos
            wait_until(lambda: console.request(VIDEO, {"action": "status"})["streams"] == [], "drained stream was not closed")
            capture = console.request(CAPTURE, {"action": "status"})
            assert capture["opened"] == capture["closed"] == capture["frames"] == capture["active"] == 0, capture
            generic_before = time.monotonic_ns() // 1000
            console.send({"op": "request", "service": "test.media-probe/1", "payload": {"action": "generic_encoder_video"},
                          "timeout_ms": 10000, "operation_id": "generic-encoder-video-test"})
            response = console.receive()
            generic_after = time.monotonic_ns() // 1000
            assert response["kind"] == "result", response
            generic = response["payload"]
            assert generic["ok"] and generic["rejected_eos"], generic
            packets = generic["packets"]
            assert len(packets) == 13 and packets[-1]["flags"] == 2, packets
            assert [packet["sequence"] for packet in packets[:-1]] == list(range(1, 13)), packets
            for packet in packets[:-1]:
                assert packet["codec"] == 1 and packet["flags"] & 4, packet
                assert bytes.fromhex(packet["bytes_hex"]) == b"FAKE-ENCODER\0" + packet["sequence"].to_bytes(8, "little"), packet
                assert generic_before <= packet["timestamp_us"] <= generic_after, packet
            wait_until(lambda: console.request(CAPTURE, {"action": "status"})["active"] == 0,
                       "generic capture lease was not closed")
            capture = console.request(CAPTURE, {"action": "status"})
            assert capture["opened"] == capture["closed"] == 1 and capture["frames"] == 12, capture
            console.eof()
        except BaseException:
            print("".join(console.errors), file=sys.stderr)
            raise
        finally:
            console.close()
    print("Passed: signed source and generic CPU encoder routes, EOS and cleanup, owner isolation, source without capture.")
    print("Fixture payload is deliberately non-decodable; decoder and playback are not qualified.")


if __name__ == "__main__":
    main()
