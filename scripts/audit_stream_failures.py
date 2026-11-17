#!/usr/bin/env python3
"""Run the Rust AST audit without adding analysis dependencies to the host.

Pass audit options through verbatim; use --help for the audit's usage. Builds
only the standalone analyzer, offline and locked. CARGO_TARGET_DIR is honored.
"""
import os
from pathlib import Path
import subprocess
import sys


def main():
    tool = Path(__file__).resolve().parent / "stream-failure-audit"
    command = ["cargo", "run", "--quiet", "--offline", "--locked",
               "--manifest-path", str(tool / "Cargo.toml"), "--", *sys.argv[1:]]
    return subprocess.run(command, env=os.environ.copy(), check=False).returncode


if __name__ == "__main__":
    sys.exit(main())
