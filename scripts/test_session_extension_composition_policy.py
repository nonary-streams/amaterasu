#!/usr/bin/env python3
"""Validate split participant and registry policy assembly."""
import copy
import unittest

import check_session_extension_composition as check
import session_extension_composition_policy as policy


def selection(provider, revision, methods):
    service = provider + "/2"
    descriptor = {"provider": provider, "revision": revision, "actions": [],
                  "settings_validate": policy.method_ref(provider, "settings_validate"),
                  "options": [{"id": "enabled", "definition": {"type": "boolean", "default": False}}]}
    component = {"id": provider, "runtime": False, "requires": {"amaterasu.runtime": "0.1.0"},
                 "requested": {"provide": [service], "call": []}}
    approvals = [{"provider": provider, "callers": ["untrusted"], "method": {
        "service": service, "method": name, "interface_major": 2, "interface_minor": 0}}
        for name in sorted(methods)]
    return {"descriptor": descriptor, "descriptor_digest": "a" * 64,
            "component": component, "approvals": approvals}


class CompositionPolicyTest(unittest.TestCase):
    def setUp(self):
        self.display = selection(check.DISPLAY, 2, check.DISPLAY_METHODS)
        self.owner = selection(check.OWNER, 1, check.OWNER_METHODS)
        self.selected = [self.display, self.owner]

    def test_owner_precedes_dependent_and_default_actions_are_disabled(self):
        before = copy.deepcopy(self.selected)
        result = policy.provider_entries(self.selected)
        self.assertEqual(self.selected, before)
        self.assertEqual([entry["provider"] for entry in result["participants"]], [check.OWNER, check.DISPLAY])
        self.assertEqual(result["participants"][1]["depends_on"], [check.OWNER])
        self.assertEqual(result["participants"][0]["binding_validate_method"], policy.method_ref(check.OWNER, "binding_validate"))
        self.assertEqual(result["input_actions"], [])
        self.assertEqual(result["action_grants"], [])
        self.assertTrue(all("untrusted" not in approval["callers"] for approval in result["api_methods"]))

    def test_action_requires_exact_contract(self):
        with self.assertRaisesRegex(ValueError, "does not advertise"):
            policy.provider_entries(self.selected, True)
        self.owner["descriptor"]["actions"] = [{"id": check.OWNER + "/remove-owned", "execution": "session_mutation",
            "allowed_targets": ["session", "stream"], "arguments": {},
            "plan": policy.method_ref(check.OWNER, "action_plan"),
            "execute": policy.method_ref(check.OWNER, "action_execute"),
            "status": policy.method_ref(check.OWNER, "action_status")}]
        result = policy.provider_entries(self.selected, True)
        self.assertEqual(result["input_actions"], [check.OWNER + "/remove-owned"])
        self.assertEqual(len(result["action_grants"]), 3)
        self.owner["descriptor"]["actions"][0]["allowed_targets"] = ["host"]
        with self.assertRaisesRegex(ValueError, "unsupported owner action"):
            policy.provider_entries(self.selected, True)

    def test_missing_duplicate_and_mismatched_provider_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "both selected"):
            policy.provider_entries([self.owner])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            policy.provider_entries([self.owner, self.owner])
        self.owner["component"]["requested"]["provide"] = [check.OWNER + "/1"]
        with self.assertRaisesRegex(ValueError, "selected v2"):
            policy.provider_entries(self.selected)


if __name__ == "__main__":
    unittest.main()
