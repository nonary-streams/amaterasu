#!/usr/bin/env python3
"""Independently decode a controlled virtual-output capture and verify its pattern.

Validate alternating synthetic fixture frames. No display or hardware capture is used.
"""
import argparse
import json
import os
import subprocess
import sys

MAX_BYTES = 2_000_000
MAX_FRAMES = 16

def verify_pixels(raw, width, height):
    size = width * height
    if len(raw) % size or not 3 <= len(raw) // size <= MAX_FRAMES:
        raise ValueError("invalid decoded frame count or dimensions")
    signs = set()
    margin = max(4, width // 16)
    top, bottom = height // 4, 3 * height // 4
    for start in range(0, len(raw), size):
        frame = raw[start:start + size]
        left = [frame[y * width + x] for y in range(top, bottom) for x in range(margin, width // 2 - margin)]
        right = [frame[y * width + x] for y in range(top, bottom) for x in range(width // 2 + margin, width - margin)]
        contrast = sum(left) / len(left) - sum(right) / len(right)
        if abs(contrast) >= 80:
            signs.add(1 if contrast > 0 else -1)
    if signs != {-1, 1}:
        raise ValueError("both controlled alternating panel states were not observed")
    return len(raw) // size

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("path")
    parser.add_argument("width", type=int)
    parser.add_argument("height", type=int)
    args = parser.parse_args()
    if not (64 <= args.width <= 1920 and 48 <= args.height <= 1080 and args.width % 2 == args.height % 2 == 0):
        raise ValueError("invalid dimensions")
    if not 0 < os.stat(args.path).st_size <= MAX_BYTES:
        raise ValueError("invalid encoded file size")
    probe = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height", "-of", "json", args.path], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=True)
    streams = json.loads(probe.stdout)["streams"]
    if len(streams) != 1 or streams[0] != {"codec_name": "h264", "width": args.width, "height": args.height}:
        raise ValueError("decoded dimensions differ from authorized output")
    result = subprocess.run(["ffmpeg", "-nostdin", "-xerror", "-hide_banner", "-loglevel", "error", "-threads", "1", "-f", "h264", "-i", args.path, "-frames:v", str(MAX_FRAMES), "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15, check=True)
    count = verify_pixels(result.stdout, args.width, args.height)
    print(f"Verified {count} independently decoded frames against alternating panels")

if __name__ == "__main__":
    try: main()
    except (ValueError, OSError, subprocess.SubprocessError) as exc:
        print(f"Verification failed: {exc}", file=sys.stderr)
        sys.exit(1)
