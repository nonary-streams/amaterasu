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
            prepare.validated_runtime_configuration()
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
                    (root / "bootstrap.json").write_text(json.dumps({"policy": {
                        "principals": {identity: {"call": []} for identity in identities},
                        "settings": {legacy.NVHTTP: {"streaming": {"capture": {}}}}, "api_methods": []}}))
                if arguments[1] == "pack-linux-session-extensions":
                    self.assertEqual(arguments.count("--external-component"), 2)
                    for i in (arguments.index("--external-component"), len(arguments) - 5):
                        self.assertTrue(Path(arguments[i + 1]).is_file())
                        self.assertTrue(Path(arguments[i + 2]).is_file())
                return ""
            with mock.patch.object(prepare.legacy, "run", side_effect=run), mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": "/new/journal", "operation_grants": {}}):
                prepare.pack_profile(Path("release"), root, directory, package, 1, inputs, [])
            self.assertEqual([call[1] for call in calls], ["init-linux-session", "pack-linux-session-extensions", "verify"])
            self.assertTrue(json.loads((root / "bootstrap.json").read_text())["policy"]["settings"][legacy.NVHTTP]["session_lifecycle_required"])

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
            with mock.patch.object(prepare, "validated_runtime_configuration", return_value={}):
                with self.assertRaisesRegex(ValueError, "artifact changed"):
                    prepare.pack_profile(Path("release"), root, directory, directory / "package", 1, inputs, [])
            self.assertFalse(root.exists())


if __name__ == "__main__":
    unittest.main()
