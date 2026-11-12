#!/usr/bin/env python3
"""Qualify two separately compiled neutral consumers against the local adapter."""
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[1]
target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
fixtures = target / "duplex-consumers"
fixtures.mkdir(parents=True, exist_ok=True)
env = os.environ.copy()
for name in ("identity", "reverse"):
    library = fixtures / f"{name}.so"
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC",
                    "-I", str(root / "sdk/include"), str(root / f"fixtures/duplex-consumers/{name}.c"),
                    "-o", str(library)], check=True, cwd=root)
    env[f"AMATERASU_DUPLEX_{name.upper()}"] = str(library)
subprocess.run(["cargo", "test", "--offline", "--locked", "-p", "amaterasu-runtime",
                "independently_built_neutral_duplex_consumers", "--", "--ignored"],
               check=True, cwd=root, env=env)
