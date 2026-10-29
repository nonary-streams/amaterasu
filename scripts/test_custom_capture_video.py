#!/usr/bin/env python3
"""Signed custom capture through real video and x264, with independent decode."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from test_component_updates import CARGO, ComponentHarness, run
from test_host_service import Console
from test_public_software_stream import CAPTURE, HEIGHT, SOURCE_FRAMES, WIDTH, configure, wait_ready, wait_until


def main():
    target = Path(os.environ["CARGO_TARGET_DIR"])
    assert target.is_absolute(), "use an isolated absolute Cargo target"
    run(CARGO, "build", "--workspace", "--locked", "--offline", "--features", "amaterasu-encode/software-x264")
    with tempfile.TemporaryDirectory(prefix="amaterasu-custom-video-") as temporary:
        directory = Path(temporary)
        harness = ComponentHarness(directory)
        configure(harness, custom_capture=True, direct=True)
        console = Console(harness)
        try:
            wait_ready(console)
            console.send({"op": "request", "service": "test.media-probe/1", "payload": {"action": "custom_video"},
                          "timeout_ms": 10000, "operation_id": "custom-video-probe"})
            response = console.receive()
            assert response["kind"] == "result", response
            result = response["payload"]
            assert result["ok"] and result["access_units"] >= SOURCE_FRAMES, result
            assert len(result["video"]["streams"]) == 1, result["video"]
            assert result["video"]["streams"][0]["capture_provider"] == "custom", result["video"]
            wait_until(lambda: console.request(CAPTURE, {"action": "status"})["active"] == 0,
                       "custom capture lease was not released after close")
            capture = console.request(CAPTURE, {"action": "status"})
            assert capture["callers"]["amaterasu.video"] == {"opened": 1, "closed": 1, "frames": SOURCE_FRAMES, "active": 0}, capture
            encoded = directory / "custom.h264"
            encoded.write_bytes(bytes.fromhex(result["h264_hex"]))
            decoded = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-threads", "1", "-f", "h264", "-i", str(encoded), "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"], capture_output=True, timeout=15)
            assert decoded.returncode == 0, decoded.stderr.decode(errors="replace")
            frame_size = WIDTH * HEIGHT
            assert len(decoded.stdout) >= SOURCE_FRAMES * frame_size
            contrasts = []
            for index in range(SOURCE_FRAMES):
                pixels = decoded.stdout[index * frame_size:(index + 1) * frame_size]
                left = sum(pixels[row * WIDTH + column] for row in range(HEIGHT) for column in range(4, WIDTH // 2 - 4))
                right = sum(pixels[row * WIDTH + column] for row in range(HEIGHT) for column in range(WIDTH // 2 + 4, WIDTH - 4))
                contrasts.append(left - right)
                assert abs(left - right) > 100 * HEIGHT * (WIDTH // 2 - 8), contrasts
            assert all(a * b < 0 for a, b in zip(contrasts, contrasts[1:])), contrasts
            console.eof()
        except BaseException:
            print("".join(console.errors), file=sys.stderr)
            raise
        finally:
            console.close()
    print(json.dumps({"custom_provider": "test.custom-capture/1", "decoded_frames": len(decoded.stdout) // frame_size, "capture": capture}, sort_keys=True))


if __name__ == "__main__":
    main()
