#!/usr/bin/env python3
"""Create a local processing-SDK workspace; never modifies an existing path."""
from pathlib import Path
import shutil
import sys

ROOT = Path(__file__).resolve().parent.parent
CRATES = (
    "abi", "timing-metrics", "data-plane", "configuration-schema", "extension-api", "media",
    "contracts", "network-api", "process-api", "network-sockets", "module-sdk",
    # The test kit runs a module against the real control runtime in a test.
    "runtime", "module-testkit",
)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: create-processing-sdk.py NEW_DIRECTORY")
    destination = Path(sys.argv[1]).absolute()
    destination.mkdir(parents=False, exist_ok=False)
    try:
        for name in CRATES:
            shutil.copytree(ROOT / "crates" / name, destination / "crates" / name)
        shutil.copytree(ROOT / "sdk" / "include", destination / "sdk" / "include")
        shutil.copytree(ROOT / "sdk" / "examples" / "frame-processor",
                        destination / "modules" / "example-frame-processor")
        helper = destination / "crates" / "rtx-runtime"
        helper.mkdir()
        shutil.copytree(ROOT / "crates" / "rtx-runtime" / "src", helper / "src")
        # The runtime helper is a separate workspace and has no SDK dependency.
        # Its host-only integration tests are not part of this library bundle.
        (helper / "Cargo.toml").write_text('''[package]
name = "amaterasu-rtx-runtime"
version = "0.1.0"
edition = "2024"
rust-version = "1.89"
[workspace]
[dependencies]
sha2 = "0.10"
flate2 = { version = "1", features = ["rust_backend"] }
libc = "0.2"
getrandom = "0.3"
ureq = { version = "=3.4.2", default-features = false, features = ["rustls", "platform-verifier"] }
zip = { version = "2.4.2", default-features = false, features = ["deflate-flate2", "flate2"] }
''')
        members = [f'"crates/{name}"' for name in CRATES] + ['"modules/*"']
        deps = [f'amaterasu-{name} = {{ path = "crates/{name}" }}' for name in CRATES]
        (destination / "Cargo.toml").write_text('''[workspace]
resolver = "2"
members = [''' + ", ".join(members) + ''']
exclude = ["crates/rtx-runtime"]
[workspace.package]
edition = "2024"
rust-version = "1.89"
version = "0.1.0"
[workspace.dependencies]
''' + "\n".join(deps) + '''
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
getrandom = "0.3"
libloading = "0.8"
[workspace.lints.rust]
unsafe_op_in_unsafe_fn = "deny"
''')
        shutil.copy2(ROOT / "Cargo.lock", destination / "Cargo.lock")
        (destination / "README.md").write_text('''# Local frame-processing SDK

This workspace contains the Rust SDK, its contract crates, C headers, and an
independently buildable CPU pass-through processor in `modules/example-frame-processor`.
`crates/module-testkit` runs a module against the control runtime in
`crates/runtime` inside an ordinary `cargo test`.
`crates/rtx-runtime` is a separate Cargo workspace: it is an NVIDIA-specific
helper, not a dependency of the generic SDK. A separately cloned RTX HDR
checkout can be placed in `modules/rtx-hdr` and built from this workspace.
''')
    except BaseException:
        shutil.rmtree(destination)
        raise
    print(destination)


if __name__ == "__main__":
    main()
