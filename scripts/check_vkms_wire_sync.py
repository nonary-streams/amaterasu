#!/usr/bin/env python3
"""Require the embedded provider and installed helper to share wire sources.

The provider submodule keeps an independently buildable copy. When changing
these files, update and publish both copies together and advance the submodule
pin only after testing the helper and capture provider as a pair.
"""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parent.parent
PARENT = ROOT / "crates/vkms-frame-helper/src"
PROVIDER = ROOT / "modules/capture-vkms/helper-wire/src"
SHARED_SOURCES = (
    "lib.rs", "protocol.rs", "channel.rs", "client.rs", "authority.rs",
    "process.rs", "drm_node.rs",
)


def main() -> int:
    mismatched = []
    for name in SHARED_SOURCES:
        try:
            if (PARENT / name).read_bytes() != (PROVIDER / name).read_bytes():
                mismatched.append(name)
        except OSError as error:
            print(f"Cannot read VKMS shared source {name}: {error}", file=sys.stderr)
            return 1
    if mismatched:
        print(
            "VKMS helper and provider sources differ: " + ", ".join(mismatched)
            + ". Update and validate both copies before advancing the submodule pin.",
            file=sys.stderr,
        )
        return 1
    print("VKMS helper and provider shared sources match")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
