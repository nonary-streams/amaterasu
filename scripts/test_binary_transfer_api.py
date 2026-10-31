#!/usr/bin/env python3
"""Build a separate provider DLL and exercise it through authenticated runtime endpoints."""
import argparse
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    target = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
    if not target.is_absolute():
        target = root / target
    library_name = (
        "amaterasu_data_plane_fixture.dll" if sys.platform == "win32" else
        "libamaterasu_data_plane_fixture.dylib" if sys.platform == "darwin" else
        "libamaterasu_data_plane_fixture.so"
    )
    if not args.skip_build:
        subprocess.run(
            ["cargo", "build", "--offline", "--locked", "-p", "amaterasu-data-plane-fixture"],
            cwd=root, check=True,
        )
    library = target / "debug" / library_name
    if not library.is_file():
        parser.error(f"provider library is missing: {library}")
    env = dict(os.environ, AMATERASU_DATA_PLANE_FIXTURE=str(library))
    subprocess.run(
        ["cargo", "test", "--offline", "--locked", "-p", "amaterasu-runtime",
         "independently_built_binary_provider", "--", "--ignored", "--nocapture"],
        cwd=root, env=env, check=True,
    )


if __name__ == "__main__":
    main()
