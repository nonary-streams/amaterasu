#!/usr/bin/env python3
"""The local update tool refuses mixed builds and reports installed drift."""
from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).parent))
from local_full_update import compiled_from, drift, foreign_artifacts, missing_backend_features  # noqa: E402


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
