#!/usr/bin/env python3
"""Safety gates and packing command for split composition."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import check_session_extension_composition as check
import prepare_session_extension_composition as prepare
import prepare_session_extension_profile as legacy
from test_session_extension_composition_policy import selection


class PrepareCompositionTest(unittest.TestCase):
    def test_runtime_gate_cannot_be_bypassed_by_configuration(self):
        with self.assertRaisesRegex(RuntimeError, "not yet qualified"):
            prepare.validated_runtime_configuration(Path("/unused"), [], False)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root, package = directory / "root", directory / "package"
            with self.assertRaisesRegex(RuntimeError, "not yet qualified"):
                prepare.pack_profile(Path("release"), root, directory, package, 1, [], [])
            self.assertFalse(root.exists())
            self.assertFalse(package.exists())

    def test_pack_uses_two_selected_immutable_snapshots(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            owner = selection(check.OWNER, 1, check.OWNER_METHODS)
            display = selection(check.DISPLAY, 2, check.DISPLAY_METHODS)
            inputs = []
            for index, selected in enumerate((owner, display)):
                library = directory / f"input-{index}.so"
                content = f"library-{index}".encode()
                library.write_bytes(content)
                inputs.append({"selected": selected, "library": library,
                               "component_bytes": json.dumps(selected["component"]).encode(),
                               "report": {"artifact_sha256": hashlib.sha256(content).hexdigest()}})
            root, package = directory / "root", directory / "package"
            calls = []
            identities = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT, legacy.MANAGEMENT, legacy.CONSOLE]
            def run(*arguments):
                calls.append(arguments)
                if arguments[1] == "init-linux-session":
                    root.mkdir()
                    (root / "data").mkdir(mode=0o700)
                    (root / "bootstrap.json").write_text(json.dumps({"policy": {
                        "principals": {identity: {"call": []} for identity in identities},
                        "settings": {legacy.NVHTTP: {"streaming": {"capture": {"capture_provider": "pipewire"}}},
                                     "amaterasu.virtual-display": {"enabled": False},
                                     "amaterasu.capture-pipewire": {"enabled": True}}, "api_methods": []}}))
                if arguments[1] == "pack-linux-session-extensions":
                    self.assertEqual(arguments.count("--external-component"), 2)
                    for i in (arguments.index("--external-component"), len(arguments) - 5):
                        self.assertTrue(Path(arguments[i + 1]).is_file())
                        self.assertTrue(Path(arguments[i + 2]).is_file())
                return ""
            with mock.patch.object(prepare.legacy, "run", side_effect=run), mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": str(root / "data/session-lifecycle/journal.json"), "operation_grants": {}}):
                prepare.pack_profile(Path("release"), root, directory, package, 1, inputs, [],
                                     enable_virtual_display=True)
            self.assertEqual([call[1] for call in calls], ["init-linux-session", "pack-linux-session-extensions", "verify"])
            self.assertTrue(json.loads((root / "bootstrap.json").read_text())["policy"]["settings"][legacy.NVHTTP]["session_lifecycle_required"])
            settings = json.loads((root / "bootstrap.json").read_text())["policy"]["settings"]
            self.assertTrue(settings["amaterasu.virtual-display"]["enabled"])
            self.assertTrue(settings["amaterasu.capture-pipewire"]["allow_named_outputs"])
            self.assertEqual(settings[legacy.NVHTTP]["streaming"]["capture"]["capture_provider"], "pipewire")
            self.assertEqual((root / "data/session-lifecycle").stat().st_mode & 0o777, 0o700)

    def test_changed_artifact_is_rejected_before_root_creation(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            owner = selection(check.OWNER, 1, check.OWNER_METHODS)
            display = selection(check.DISPLAY, 2, check.DISPLAY_METHODS)
            library = directory / "library.so"
            library.write_bytes(b"changed")
            inputs = [{"selected": selected, "library": library, "component_bytes": b"{}",
                       "report": {"artifact_sha256": hashlib.sha256(b"original").hexdigest()}}
                      for selected in (owner, display)]
            root = directory / "root"
            with mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": str(root / "data/session-lifecycle/journal.json")}):
                with self.assertRaisesRegex(ValueError, "artifact changed"):
                    prepare.pack_profile(Path("release"), root, directory, directory / "package", 1, inputs, [])
            self.assertFalse(root.exists())

    def test_forwarded_capture_selection_is_rejected_before_initialization(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root = directory / "root"
            with mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": str(root / "data/session-lifecycle/journal.json")}):
                with self.assertRaisesRegex(ValueError, "composition profile options"):
                    prepare.pack_profile(Path("release"), root, directory, directory / "package", 1, [],
                                         ["--enable-virtual-display"])
            self.assertFalse(root.exists())

    def test_journal_directory_rejects_symlink_and_nonprivate_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root, outside = base / "root", base / "outside"
            root.mkdir(mode=0o700)
            outside.mkdir(mode=0o700)
            (root / "data").symlink_to(outside, target_is_directory=True)
            with self.assertRaises(OSError):
                prepare.create_journal_directory(root, root.resolve())
            self.assertFalse((outside / "session-lifecycle").exists())
            (root / "data").unlink()
            (root / "data").mkdir(mode=0o755)
            with self.assertRaisesRegex(ValueError, "private"):
                prepare.create_journal_directory(root, root.resolve())
            self.assertFalse((root / "data/session-lifecycle").exists())

    def test_journal_path_outside_fresh_root_is_rejected_before_creation(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root = directory / "root"
            with mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": "/outside/journal.json"}):
                with self.assertRaisesRegex(ValueError, "new root's private directory"):
                    prepare.pack_profile(Path("release"), root, directory, directory / "package", 1, [], [])
            self.assertFalse(root.exists())


if __name__ == "__main__":
    unittest.main()
