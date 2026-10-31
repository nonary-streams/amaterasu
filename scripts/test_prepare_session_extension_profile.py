#!/usr/bin/env python3
"""Policy admission checks for the opt-in local extension composition."""
from __future__ import annotations

import copy
import tempfile
from pathlib import Path
import unittest

import prepare_session_extension_profile as profile


class ProfileActionPolicyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="amaterasu-profile-policy-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "data").mkdir()
        principals = [profile.NVHTTP, profile.RTSP, profile.STREAM, profile.INPUT,
                      profile.MANAGEMENT, profile.CONSOLE]
        self.bootstrap = {"policy": {"principals": {identity: {"call": []} for identity in principals},
                                     "settings": {profile.NVHTTP: {"streaming": {"capture": {}}}},
                                     "api_methods": []}}
        self.descriptor = {"provider": profile.PROVIDER, "options": [{"id": "enabled",
                            "definition": {"default": False}}], "actions": []}
        self.component = {"id": profile.PROVIDER, "requested": {"provide": [profile.PROVIDER_SERVICE]}}
        self.approvals = [{"provider": profile.PROVIDER,
                           "method": {"service": profile.PROVIDER_SERVICE, "method": name,
                                      "interface_major": 1}} for name in
                          ("settings_validate", "resource_plan", "lifecycle_callback",
                           "lifecycle_status", "binding_validate")]

    def test_explicit_action_requires_exact_advertisement_and_preserves_default(self) -> None:
        with self.assertRaisesRegex(ValueError, "does not advertise"):
            profile.prepare(copy.deepcopy(self.bootstrap), self.root, self.descriptor, "digest",
                            copy.deepcopy(self.approvals), self.component, True)
        action_id = profile.PROVIDER + "/remove-owned"
        self.descriptor["actions"] = [{"id": action_id, "execution": "session_mutation",
            "allowed_targets": ["session", "stream"], "arguments": {},
            "plan": profile.method_ref("action_plan"), "execute": profile.method_ref("action_execute"),
            "status": profile.method_ref("action_status")}]
        for name in ("action_plan", "action_execute", "action_status"):
            self.approvals.append({"provider": profile.PROVIDER, "method": {
                "service": profile.PROVIDER_SERVICE, "method": name, "interface_major": 1}})
        default_root = self.root / "default"
        (default_root / "data").mkdir(parents=True)
        default_bootstrap = copy.deepcopy(self.bootstrap)
        profile.prepare(default_bootstrap, default_root, self.descriptor, "digest",
                        copy.deepcopy(self.approvals), self.component)
        self.assertEqual(default_bootstrap["policy"]["settings"][profile.ACTIONS]["grants"], [])
        self.assertEqual(default_bootstrap["policy"]["settings"][profile.LIFECYCLE]["input_actions"], [])
        bootstrap = copy.deepcopy(self.bootstrap)
        profile.prepare(bootstrap, self.root, self.descriptor, "digest", copy.deepcopy(self.approvals),
                        self.component, True)
        settings = bootstrap["policy"]["settings"]
        self.assertEqual(settings[profile.LIFECYCLE]["input_actions"], [action_id])
        self.assertEqual({grant["principal"] for grant in settings[profile.ACTIONS]["grants"]},
                         {profile.MANAGEMENT, profile.INPUT, profile.STREAM})
        self.assertTrue(all(grant["target_kinds"] == ["session", "stream"] and
                            grant["execution_classes"] == ["session_mutation"]
                            for grant in settings[profile.ACTIONS]["grants"]))

    def test_invalid_action_target_fails_closed(self) -> None:
        self.descriptor["actions"] = [{"id": profile.PROVIDER + "/remove-owned",
            "execution": "session_mutation", "allowed_targets": ["host"], "arguments": {},
            "plan": profile.method_ref("action_plan"), "execute": profile.method_ref("action_execute"),
            "status": profile.method_ref("action_status")}]
        with self.assertRaisesRegex(ValueError, "unsupported display action contract"):
            profile.prepare(self.bootstrap, self.root, self.descriptor, "digest", self.approvals,
                            self.component, True)


if __name__ == "__main__":
    unittest.main()
