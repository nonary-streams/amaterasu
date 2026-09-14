#!/usr/bin/env python3
"""Exercise real dynamic libraries, signatures, activation, and recovery.

Uses an isolated temporary host root and development signing identity. This does
not launch applications, install services, or touch display/input devices.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


WORKSPACE = Path(__file__).resolve().parents[1]
CARGO = os.environ.get("CARGO", "cargo")


def run(*arguments: str | Path, expect_success: bool = True) -> str:
    result = subprocess.run(
        [str(argument) for argument in arguments],
        cwd=WORKSPACE,
        text=True,
        capture_output=True,
        timeout=120,
    )
    if expect_success and result.returncode:
        raise AssertionError(f"Command failed: {arguments}\n{result.stdout}\n{result.stderr}")
    if not expect_success and result.returncode == 0:
        raise AssertionError(f"Command unexpectedly succeeded: {arguments}")
    return result.stdout


def file_hash(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class ComponentHarness:
    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self.root = directory / "host"
        target = Path(os.environ.get("CARGO_TARGET_DIR", WORKSPACE / "target"))
        self.artifacts = target.resolve() / "debug"
        executable_suffix = ".exe" if os.name == "nt" else ""
        self.host = self.artifacts / f"amaterasu{executable_suffix}"
        self.publisher = self.artifacts / f"amaterasu-release{executable_suffix}"
        run(self.publisher, "init", self.root)

    def package(self, sequence: int) -> Path:
        destination = self.directory / f"release-{sequence}"
        run(self.publisher, "pack", self.root, self.artifacts, destination, str(sequence))
        run(self.publisher, "verify", self.root, destination)
        return destination

    def stage(self, package: Path, *, historical: bool = False) -> None:
        arguments = [self.publisher, "stage", self.root, package]
        if historical:
            arguments.append("--historical")
        run(*arguments)

    def probe(self, expected_revision: int) -> None:
        request = {
            "op": "request",
            "service": "example.echo/1",
            "payload": {"message": "independent module update"},
            "timeout_ms": 1000,
            "operation_id": "integration.probe",
        }
        result = json.loads(run(self.host, "probe", self.root, json.dumps(request)))
        assert result["kind"] == "result", result
        assert result["payload"]["revision"] == expected_revision, result
        assert result["payload"]["echo"] == request["payload"], result

    def state(self) -> dict:
        return json.loads((self.root / "selection.json").read_text())

    def build_fixture(self, *features: str) -> None:
        arguments = [CARGO, "build", "--offline", "-p", "amaterasu-echo-fixture"]
        if features:
            arguments.extend(["--features", ",".join(features)])
        run(*arguments)


def test_independent_update(harness: ComponentHarness) -> tuple[Path, str]:
    first_package = harness.package(1)
    harness.stage(first_package)
    harness.probe(1)
    shell_hash = file_hash(harness.host)

    harness.build_fixture("revision-two")
    harness.stage(harness.package(2))
    harness.probe(2)
    assert file_hash(harness.host) == shell_hash, "module update rebuilt the shell"
    return first_package, shell_hash


def test_broken_updates_recover(harness: ComponentHarness) -> None:
    known_good = harness.state()["active"]
    harness.build_fixture("revision-two", "fail-health")
    harness.stage(harness.package(3))
    harness.probe(2)
    assert harness.state()["active"] == known_good
    assert harness.state()["highest_sequence"] == 3

    harness.build_fixture("revision-two", "stall-tick")
    harness.stage(harness.package(4))
    harness.probe(2)
    assert harness.state()["active"] == known_good
    assert harness.state()["pending"] is None


def test_interrupted_activation(harness: ComponentHarness, first_package: Path) -> None:
    harness.stage(first_package, historical=True)
    state = harness.state()
    state["trial"] = state["pending"]
    (harness.root / "selection.json").write_text(json.dumps(state))

    harness.probe(2)
    assert harness.state()["trial"] is None
    assert harness.state()["pending"] is None


def test_historical_selection(harness: ComponentHarness, first_package: Path) -> None:
    run(harness.publisher, "stage", harness.root, first_package, expect_success=False)
    harness.stage(first_package, historical=True)
    harness.probe(1)
    assert harness.state()["highest_sequence"] == 4


def test_tampering_is_rejected(harness: ComponentHarness, first_package: Path) -> None:
    tampered = harness.directory / "tampered"
    shutil.copytree(first_package, tampered)
    manifest = json.loads((tampered / "manifest.json").read_text())
    artifact = tampered / manifest["components"][1]["file"]
    with artifact.open("ab") as output:
        output.write(b"tampered")

    run(harness.publisher, "verify", harness.root, tampered, expect_success=False)


def test_unsigned_mode(harness: ComponentHarness, first_package: Path) -> None:
    unsigned = harness.directory / "unsigned"
    shutil.copytree(first_package, unsigned)
    (unsigned / "manifest.sig").unlink()
    manifest = json.loads((unsigned / "manifest.json").read_text())
    manifest["sequence"] = 5
    manifest["components"][0]["id"] = "custom.runtime"
    manifest["components"][1]["id"] = "custom.echo"
    manifest["components"][1]["requires"] = {"custom.runtime": "0.1.0"}
    (unsigned / "manifest.json").write_text(json.dumps(manifest))
    run(harness.publisher, "verify", harness.root, unsigned, expect_success=False)

    bootstrap_path = harness.root / "bootstrap.json"
    bootstrap = json.loads(bootstrap_path.read_text())
    bootstrap["allow_unsigned"] = True
    bootstrap["policy"]["principals"]["custom.echo"] = {"provide": ["example.echo/1"]}
    bootstrap_path.write_text(json.dumps(bootstrap))
    harness.stage(unsigned)
    harness.probe(1)

    bootstrap["allow_unsigned"] = False
    bootstrap_path.write_text(json.dumps(bootstrap))
    run(harness.host, "probe", harness.root, expect_success=False)


def main() -> None:
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-integration-") as temporary:
        harness = ComponentHarness(Path(temporary))
        first_package, shell_hash = test_independent_update(harness)
        test_broken_updates_recover(harness)
        test_interrupted_activation(harness, first_package)
        test_historical_selection(harness, first_package)
        test_tampering_is_rejected(harness, first_package)
        test_unsigned_mode(harness, first_package)
        assert file_hash(harness.host) == shell_hash
        harness.build_fixture()
    print("Passed: independent update, failure/stall recovery, interrupted activation,")
    print("historical selection, tamper rejection, unsigned mode, unchanged shell hash.")


if __name__ == "__main__":
    main()
