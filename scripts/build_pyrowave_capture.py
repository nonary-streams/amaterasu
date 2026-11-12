#!/usr/bin/env python3
"""Build the independent capture provider against this worktree's draft SDK.

Published capture dependencies remain pinned. This isolated development manifest
exercises the typed SDK contracts without publishing a new SDK or modifying the
capture repository's dependency lock. It does not install or grant a helper.
"""
import argparse
import json
import os
import re
import tomllib
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test", action="store_true")
    args = parser.parse_args()
    source = ROOT / "modules/capture-vkms/capture"
    destination = ROOT / "native-deps/capture-sdk"
    destination.mkdir(parents=True, exist_ok=True)
    manifest = (source / "Cargo.toml").read_text()
    dependencies = tomllib.loads(manifest)["dependencies"]
    for package in ("amaterasu-module-sdk", "amaterasu-media"):
        dependency = dependencies[package]
        if (dependency.get("git") != "https://github.com/nonary-streams/amaterasu-sdk"
                or not re.fullmatch(r"[0-9a-f]{40}", dependency.get("rev", ""))):
            raise RuntimeError(f"Unexpected published capture SDK dependency for {package}")
        new = f'{package} = {{ path = {json.dumps(str(ROOT / "crates" / package.removeprefix("amaterasu-")))} }}'
        manifest, count = re.subn(rf'^{re.escape(package)} = \{{[^\n]*\}}$', new, manifest, flags=re.MULTILINE)
        if count != 1:
            raise RuntimeError(f"Cannot locate the exact capture SDK dependency for {package}")
    manifest = manifest.replace('path = "../helper-wire"', f'path = {json.dumps(str(source.parent / "helper-wire"))}')
    (destination / "Cargo.toml").write_text(manifest)
    for name in ("src", "build.rs", "protocols"):
        target = destination / name
        if target.is_symlink():
            if target.resolve() != (source / name).resolve():
                raise RuntimeError(f"Unexpected development source link: {target}")
        elif target.exists():
            raise RuntimeError(f"Refusing to replace development source: {target}")
        else:
            target.symlink_to(source / name, target_is_directory=(source / name).is_dir())
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(ROOT / "target/capture-sdk")
    environment["CARGO_BUILD_JOBS"] = "1"
    environment["AMATERASU_CAPTURE_TYPED_NATIVE"] = "1"
    command = ["cargo", "test" if args.test else "build", "--offline", "--manifest-path", str(destination / "Cargo.toml"),
               "--features", "driver-read-leases,linux-kwin-geometry,linux-gpu-snapshot"]
    subprocess.run(command, check=True, env=environment)
    print(ROOT / "target/capture-sdk/debug/libamaterasu_capture_vkms.so")

if __name__ == "__main__":
    main()
