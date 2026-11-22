#!/usr/bin/env python3
"""The local update tool refuses mixed builds and reports installed drift."""
from __future__ import annotations

from pathlib import Path
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parent))
from local_full_update import (  # noqa: E402
    CATALOG_DOMAIN, compiled_from, core_package, drift, foreign_artifacts,
    missing_backend_features, sign_core_bundle, source_state,
)


def component(identifier: str, file: str) -> dict:
    return {"id": identifier, "file": file, "sha256": "a" * 64}


class MixedBuildTests(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.tree = Path(directory.name) / "tree"
        self.release = self.tree / "target/release"
        self.release.mkdir(parents=True)

    def build(self, name: str, source_root: Path) -> None:
        (self.release / f"{name}.so").write_bytes(b"library")
        (self.release / f"{name}.d").write_text(
            f"{self.release}/{name}.so: {source_root}/modules/example/src/lib.rs\n")

    def test_dependency_file_names_the_source_tree(self) -> None:
        self.build("libone", self.tree)
        self.assertEqual(compiled_from(self.release / "libone.so"), self.tree)

    def test_a_complete_set_from_one_tree_is_accepted(self) -> None:
        self.build("libone", self.tree)
        self.assertEqual(foreign_artifacts(self.tree, [component("one", "libone.so")]), [])

    def test_a_library_from_another_tree_is_refused(self) -> None:
        self.build("libold", Path("/elsewhere/old-checkout"))
        problems = foreign_artifacts(self.tree, [component("old", "libold.so")])
        self.assertEqual(len(problems), 1)
        self.assertIn("/elsewhere/old-checkout", problems[0])

    def test_a_missing_library_is_refused(self) -> None:
        problems = foreign_artifacts(self.tree, [component("gone", "libgone.so")])
        self.assertIn("is missing", problems[0])


class DriftTests(unittest.TestCase):
    PROVENANCE = {"components": {"one": "a" * 64, "two": "b" * 64}, "host_sha256": "c" * 64}

    @staticmethod
    def manifest(two: str = "b" * 64) -> dict:
        return {"components": [{"id": "one", "sha256": "a" * 64}, {"id": "two", "sha256": two}]}

    def test_a_matching_install_reports_nothing(self) -> None:
        self.assertEqual(drift(self.PROVENANCE, self.manifest(), "c" * 64), [])

    def test_a_replaced_component_is_named(self) -> None:
        problems = drift(self.PROVENANCE, self.manifest(two="d" * 64), "c" * 64)
        self.assertEqual(len(problems), 1)
        self.assertIn("two", problems[0])

    def test_a_replaced_host_is_reported(self) -> None:
        self.assertIn("host", drift(self.PROVENANCE, self.manifest(), "e" * 64)[0])

    def test_a_component_added_after_install_is_reported(self) -> None:
        manifest = self.manifest()
        manifest["components"].append({"id": "three", "sha256": "f" * 64})
        self.assertIn("three", drift(self.PROVENANCE, manifest, "c" * 64)[0])


class SourceStateTests(unittest.TestCase):
    def test_dirty_submodule_sources_change_recorded_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            module = tree / "encoder"
            module.mkdir()

            def git(checkout, *arguments):
                subprocess.run(["git", "-C", str(checkout), "-c", "user.name=Source Fixture",
                                "-c", "user.email=fixture@example.test", *arguments],
                               check=True, capture_output=True)

            git(module, "init")
            source = module / "lib.rs"
            source.write_text("pub const VERSION: u8 = 1;\n")
            git(module, "add", "lib.rs")
            git(module, "commit", "-m", "Encoder fixture")
            git(tree, "init")
            (tree / ".gitmodules").write_text('[submodule "encoder"]\n\tpath = encoder\n\turl = ./encoder\n')
            git(tree, "add", ".gitmodules", "encoder")
            git(tree, "commit", "-m", "Core fixture")

            before = source_state(tree)
            source.write_text("pub const VERSION: u8 = 2;\n")
            tracked = source_state(tree)
            (module / "helper.rs").write_text("pub const LIMIT: u8 = 8;\n")
            untracked = source_state(tree)
            self.assertEqual(before["submodules"], untracked["submodules"])
            self.assertNotEqual(before["uncommitted_sha256"], tracked["uncommitted_sha256"])
            self.assertNotEqual(tracked["uncommitted_sha256"], untracked["uncommitted_sha256"])


class CoreBundleTests(unittest.TestCase):
    def test_local_core_catalog_keeps_independent_plugin_listings(self):
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            manifest = {"components": [{"id": "amaterasu.runtime", "version": "0.2.1"}],
                        "expires_unix": 1, "target": "linux-x86_64"}
            key = Ed25519PrivateKey.generate()
            plugin = {"slot": "example.plugin", "implementation": "example-plugin",
                      "version": "1.0.0", "kind": "native_plugin", "manifest_sha256": "a" * 64}
            package = core_package(work, manifest)
            package.mkdir(parents=True)
            digest, catalog_digest = sign_core_bundle(package, manifest, (key, "b" * 64, 0, [plugin]))
            raw = (work / "bundle/catalog.json").read_bytes()
            key.public_key().verify(bytes.fromhex((work / "bundle/catalog.sig").read_text()),
                                    CATALOG_DOMAIN + raw)
            catalog = json.loads(raw)
            self.assertEqual(catalog_digest, hashlib.sha256(raw).hexdigest())
            self.assertEqual(catalog["repository"], "amaterasu")
            self.assertEqual(catalog["candidates"], [
                {"slot": "amaterasu.runtime", "implementation": "amaterasu-core",
                 "version": "0.2.1", "kind": "native_set", "manifest_sha256": digest},
                plugin,
            ])


class NativeBackendTests(unittest.TestCase):
    def test_enabled_linux_display_rejects_same_tree_build_without_native_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            artifact = tree / "target/release/libamaterasu_virtual_display.so"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"unsupported platform implementation")
            artifact.with_suffix(".d").write_text(
                f"{artifact}: {tree}/modules/virtual-display/src/lib.rs\n")
            entry = component("amaterasu.virtual-display", artifact.name)
            self.assertEqual(foreign_artifacts(tree, [entry]), [])
            bootstrap = {"policy": {"settings": {entry["id"]: {"enabled": True}}}}
            manifest = {"target": "x86_64-unknown-linux-gnu", "components": [entry]}
            artifacts = {entry["id"]: artifact}
            self.assertIn("linux-libvirtualdisplay", missing_backend_features(bootstrap, manifest, artifacts)[0])
            artifact.write_bytes(b"native library\0AMATERASU_LINUX_VDD=linked-backend\0")
            self.assertEqual(missing_backend_features(bootstrap, manifest, artifacts), [])

    def test_disabled_or_windows_display_does_not_require_linux_backend(self):
        for enabled, target in [(False, "x86_64-unknown-linux-gnu"), (True, "x86_64-pc-windows-msvc")]:
            bootstrap = {"policy": {"settings": {"amaterasu.virtual-display": {"enabled": enabled}}}}
            self.assertEqual(missing_backend_features(bootstrap, {"target": target}, {}), [])


if __name__ == "__main__":
    unittest.main()
