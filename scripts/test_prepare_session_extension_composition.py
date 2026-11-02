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
    def test_invalid_selections_fail_before_creating_paths(self):
        with self.assertRaisesRegex(ValueError, "both selected providers"):
            prepare.validated_runtime_configuration(Path("/unused"), [], False)
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root, package = directory / "root", directory / "package"
            with self.assertRaisesRegex(ValueError, "both selected providers"):
                prepare.pack_profile(Path("release"), root, directory, package, 1, [], [])
            self.assertFalse(root.exists())
            self.assertFalse(package.exists())

    def test_runtime_configuration_pins_graph_and_rejects_changed_schema(self):
        owner = selection(check.OWNER, 1, check.OWNER_METHODS)
        display = selection(check.DISPLAY, 2, check.DISPLAY_METHODS)
        target, control = b'{"target":true}\n', b'{"control":true}\n'
        selected = [{"selected": owner, "schemas": {"display-target-v1.json": target,
                    "display-control-v1.json": control}}, {"selected": display}]
        with mock.patch.object(check, "TARGET_DIGEST", hashlib.sha256(target).hexdigest()), \
             mock.patch.object(check, "CONTROL_DIGEST", hashlib.sha256(control).hexdigest()):
            settings = prepare.validated_runtime_configuration(Path("/fresh"), selected, False)
            self.assertEqual(settings["control_policies"][0]["dependent_provider"], check.DISPLAY)
            self.assertEqual(settings["finalize_policies"][0]["owner_provider"], check.OWNER)
            self.assertIn("lookup_stream_closure", settings["operation_grants"][check.OWNER])
            self.assertIn("finish_input_action", settings["operation_grants"][legacy.INPUT])
            self.assertNotIn("describe_action_target", settings["operation_grants"][check.OWNER])
            action = {"id": check.OWNER + "/remove-owned", "execution": "session_mutation",
                      "allowed_targets": ["stream"], "arguments": {}, "required_capabilities": [],
                      "plan": prepare.composition.method_ref(check.OWNER, "action_plan"),
                      "execute": prepare.composition.method_ref(check.OWNER, "action_execute"),
                      "status": prepare.composition.method_ref(check.OWNER, "action_status")}
            owner["descriptor"]["actions"] = [action]
            for name in sorted(check.ACTION_METHODS):
                approval = json.loads(json.dumps(owner["approvals"][0]))
                approval["method"]["method"] = name
                owner["approvals"].append(approval)
            enabled = prepare.validated_runtime_configuration(Path("/fresh"), selected, True)
            self.assertIn("describe_action_target", enabled["operation_grants"][check.OWNER])
            self.assertIn("rebind_action_plan", enabled["operation_grants"][legacy.ACTIONS])
            selected[0]["schemas"]["display-target-v1.json"] += b" "
            with self.assertRaisesRegex(ValueError, "schema bytes"):
                prepare.validated_runtime_configuration(Path("/fresh"), selected, False)

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
            identities = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT, legacy.MANAGEMENT, legacy.CONSOLE, legacy.LAUNCH]
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
            with mock.patch.object(prepare.legacy, "run", side_effect=run), mock.patch.object(prepare, "validated_runtime_configuration", return_value={"journal_path": str(root / "data/session-lifecycle/journal.json"), "operation_grants": {check.OWNER: ["lookup_control_baseline", "lookup_stream_closure"], prepare.legacy.INPUT: ["finish_input_action"]}}):
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
