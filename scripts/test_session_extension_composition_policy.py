#!/usr/bin/env python3
"""Validate split participant and registry policy assembly."""
import copy
import hashlib
import unittest
from unittest import mock

import check_session_extension_composition as check
import session_extension_composition_policy as policy
import prepare_session_extension_profile as legacy


def selection(provider, revision, methods):
    service = provider + "/2"
    descriptor = {"provider": provider, "revision": revision, "actions": [],
                  "settings_validate": policy.method_ref(provider, "settings_validate"),
                  "options": [{"id": "enabled", "definition": {"type": "boolean", "default": False}}]}
    component = {"id": provider, "runtime": False, "requires": {"amaterasu.runtime": "0.1.0"},
                 "requested": {"provide": [service], "call": sorted(check.PROVIDER_CALLS[provider])}}
    approvals = [{"provider": provider, "callers": ["untrusted"], "method": {
        "service": service, "method": name, "interface_major": 2, "interface_minor": 0, "privilege": "user",
        "request": {"required": {}, "optional": {}, "allow_extra": False},
        "response": {"required": {}, "optional": {}, "allow_extra": False}}}
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
        self.assertTrue(all(entry["contract_version"] == 2 for entry in result["participants"]))
        for participant in result["participants"]:
            self.assertEqual(participant["cleanup_methods"], [policy.method_ref(participant["provider"], "lifecycle_callback")])
        self.assertEqual(result["participants"][0]["binding_validate_method"], policy.method_ref(check.OWNER, "binding_validate"))
        self.assertEqual(result["input_actions"], [])
        self.assertEqual(result["action_grants"], [])
        self.assertTrue(all("untrusted" not in approval["callers"] for approval in result["api_methods"]))

    def test_action_requires_exact_contract(self):
        with self.assertRaisesRegex(ValueError, "does not advertise"):
            policy.provider_entries(self.selected, True)
        self.owner["descriptor"]["actions"] = [{"id": check.OWNER + "/remove-owned", "execution": "session_mutation",
            "allowed_targets": ["stream"], "arguments": {}, "required_capabilities": [],
            "plan": policy.method_ref(check.OWNER, "action_plan"),
            "execute": policy.method_ref(check.OWNER, "action_execute"),
            "status": policy.method_ref(check.OWNER, "action_status")}]
        for name in sorted(check.ACTION_METHODS):
            approval = copy.deepcopy(self.owner["approvals"][0])
            approval["method"]["method"] = name
            self.owner["approvals"].append(approval)
        result = policy.provider_entries(self.selected, True)
        self.assertEqual(result["input_actions"], [check.OWNER + "/remove-owned"])
        self.assertEqual(len(result["action_grants"]), 3)
        self.assertTrue(all(grant["target_kinds"] == ["stream"] for grant in result["action_grants"]))
        self.owner["descriptor"]["actions"][0]["required_capabilities"] = ["unexpected"]
        with self.assertRaisesRegex(ValueError, "unsupported owner action"):
            policy.provider_entries(self.selected, True)
        self.owner["descriptor"]["actions"][0]["required_capabilities"] = []
        self.owner["descriptor"]["actions"][0]["allowed_targets"] = ["session", "stream"]
        with self.assertRaisesRegex(ValueError, "unsupported owner action"):
            policy.provider_entries(self.selected, True)
        self.owner["descriptor"]["actions"][0]["allowed_targets"] = ["host"]
        with self.assertRaisesRegex(ValueError, "unsupported owner action"):
            policy.provider_entries(self.selected, True)

    def test_registered_but_unadvertised_actions_remain_unauthorized(self):
        for name in sorted(check.ACTION_METHODS):
            approval = copy.deepcopy(self.owner["approvals"][0])
            approval["method"]["method"] = name
            self.owner["approvals"].append(approval)
        result = policy.provider_entries(self.selected)
        self.assertEqual(result["input_actions"], [])
        self.assertEqual(result["action_grants"], [])
        with self.assertRaisesRegex(ValueError, "does not advertise"):
            policy.provider_entries(self.selected, True)

    def test_action_profile_requires_owner_grants_and_full_call_chain(self):
        self.owner["descriptor"]["actions"] = [{"id": check.OWNER + "/remove-owned", "execution": "session_mutation",
            "allowed_targets": ["stream"], "arguments": {}, "required_capabilities": [],
            "plan": policy.method_ref(check.OWNER, "action_plan"),
            "execute": policy.method_ref(check.OWNER, "action_execute"),
            "status": policy.method_ref(check.OWNER, "action_status")}]
        for name in sorted(check.ACTION_METHODS):
            approval = copy.deepcopy(self.owner["approvals"][0])
            approval["method"]["method"] = name
            self.owner["approvals"].append(approval)
        entries = policy.provider_entries(self.selected, True)
        identities = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT,
                      legacy.MANAGEMENT, legacy.CONSOLE]
        base = {"policy": {"principals": {identity: {"call": []} for identity in identities},
                           "settings": {legacy.NVHTTP: {"streaming": {"capture": {}}}},
                           "api_methods": []}}
        lifecycle = {"operation_grants": {check.OWNER: ["verify_action_effect", "check_action_effect"]}}
        with self.assertRaisesRegex(ValueError, "owner action operation grants"):
            policy.build_bootstrap(base, self.selected, entries, lifecycle, True)
        lifecycle["operation_grants"][check.OWNER] = sorted(policy.ACTION_OWNER_OPERATIONS)
        with self.assertRaisesRegex(ValueError, "action-service operation grants"):
            policy.build_bootstrap(base, self.selected, entries, lifecycle, True)
        lifecycle["operation_grants"][legacy.ACTIONS] = sorted(policy.ACTION_COORDINATOR_OPERATIONS)
        bootstrap, calls = policy.build_bootstrap(base, self.selected, entries, lifecycle, True)
        for identity in (legacy.INPUT, legacy.ACTIONS, legacy.STREAM):
            if identity != legacy.ACTIONS:
                self.assertIn(legacy.ACTIONS_SERVICE, calls[identity])
            self.assertIn(legacy.LIFECYCLE_SERVICE, calls[identity])
            self.assertIn(check.OWNER + "/2", calls[identity])
            self.assertIn("amaterasu.virtual-display/1", calls[identity])
        self.assertIn(legacy.LIFECYCLE_SERVICE, calls[legacy.LIFECYCLE])
        self.assertIn("amaterasu.input/1", calls[legacy.ACTIONS])
        self.assertEqual(bootstrap["policy"]["settings"][legacy.LIFECYCLE]["input_actions"],
                         [check.OWNER + "/remove-owned"])
        disabled_entries = policy.provider_entries(self.selected)
        with self.assertRaisesRegex(ValueError, "action authorization"):
            policy.build_bootstrap(base, self.selected, disabled_entries, lifecycle, True)

    def test_missing_duplicate_and_mismatched_provider_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "both selected"):
            policy.provider_entries([self.owner])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            policy.provider_entries([self.owner, self.owner])
        self.owner["component"]["requested"]["provide"] = [check.OWNER + "/1"]
        with self.assertRaisesRegex(ValueError, "selected v2"):
            policy.provider_entries(self.selected)

    def test_bootstrap_assembly_preserves_existing_base_and_refuses_old_extension(self):
        identities = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT,
                      legacy.MANAGEMENT, legacy.CONSOLE]
        base = {"policy": {"principals": {identity: {"call": []} for identity in identities},
                           "settings": {legacy.NVHTTP: {"streaming": {"capture": {}}},
                                        "amaterasu.launch": {"retirement_observers": ["other.observer"]}},
                           "api_methods": []}}
        before = copy.deepcopy(base)
        entries = policy.provider_entries(self.selected)
        lifecycle = {"journal_path": "/new-root/data/journal.json", "operation_grants": {}}
        assembled, calls = policy.build_bootstrap(base, self.selected, entries, lifecycle)
        self.assertEqual(base, before)
        settings = assembled["policy"]["settings"]
        self.assertEqual(settings[legacy.LIFECYCLE]["participants"][1]["depends_on"], [check.OWNER])
        self.assertTrue(settings[legacy.NVHTTP]["session_lifecycle_required"])
        self.assertEqual(settings[legacy.NVHTTP]["streaming"]["capture"]["capture_provider"], "pipewire")
        self.assertEqual(settings["amaterasu.launch"]["retirement_observers"], ["other.observer", legacy.LIFECYCLE])
        self.assertIn(check.OWNER + "/2", calls[legacy.LIFECYCLE])
        self.assertIn(check.DISPLAY + "/2", calls[legacy.REGISTRY])
        base["policy"]["settings"][legacy.REGISTRY] = {"descriptors": [{"old": True}]}
        with self.assertRaisesRegex(ValueError, "explicit migration"):
            policy.build_bootstrap(base, self.selected, entries, lifecycle)
        self.assertEqual(base["policy"]["settings"][legacy.REGISTRY], {"descriptors": [{"old": True}]})

    def test_bootstrap_refuses_duplicate_approvals_and_graph_override(self):
        identities = [legacy.NVHTTP, legacy.RTSP, legacy.STREAM, legacy.INPUT,
                      legacy.MANAGEMENT, legacy.CONSOLE]
        base = {"policy": {"principals": {identity: {"call": []} for identity in identities},
                           "settings": {legacy.NVHTTP: {"streaming": {"capture": {}}}},
                           "api_methods": []}}
        entries = policy.provider_entries(self.selected)
        base["policy"]["api_methods"].append(copy.deepcopy(entries["api_methods"][0]))
        with self.assertRaisesRegex(ValueError, "approval conflicts"):
            policy.build_bootstrap(base, self.selected, entries, {})
        base["policy"]["api_methods"] = []
        with self.assertRaisesRegex(ValueError, "must not override"):
            policy.build_bootstrap(base, self.selected, entries, {"participants": []})
        base["policy"]["settings"][legacy.NVHTTP]["streaming"]["capture"]["capture_provider"] = "kms"
        with self.assertRaisesRegex(ValueError, "capture selection conflicts"):
            policy.build_bootstrap(base, self.selected, entries, {})
        base["policy"]["settings"][legacy.NVHTTP]["streaming"]["capture"]["capture_provider"] = "pipewire"
        base["policy"]["settings"][legacy.NVHTTP]["session_lifecycle_required"] = False
        with self.assertRaisesRegex(ValueError, "lifecycle configuration conflicts"):
            policy.build_bootstrap(base, self.selected, entries, {})

    def test_resource_scoped_graph_pins_owner_methods_and_schema_bytes(self):
        target, control = b'{"target":true}\n', b'{"control":true}\n'
        names = {"control": "resource_control", "control_status": "resource_status",
                 "settlement": "resource_control_settle",
                 "finalize": "resource_finalize", "finalize_status": "resource_status"}
        approvals = copy.deepcopy(self.owner["approvals"])
        for name in set(names.values()) - check.OWNER_METHODS:
            approval = copy.deepcopy(approvals[0])
            approval["method"]["method"] = name
            approvals.append(approval)
        with mock.patch.object(check, "TARGET_DIGEST", hashlib.sha256(target).hexdigest()), \
             mock.patch.object(check, "CONTROL_DIGEST", hashlib.sha256(control).hexdigest()):
            graph = policy.lifecycle_graph(approvals, target, control, names)
            self.assertEqual(graph["control_policies"][0]["domain_scope"], "resource")
            self.assertEqual(graph["control_policies"][0]["mutation_domains"], [])
            self.assertEqual(graph["finalize_policies"][0]["domain_scope"], "resource")
            self.assertEqual(graph["schema_documents"][0]["document"].encode(), target)
            with self.assertRaisesRegex(ValueError, "schema bytes"):
                policy.lifecycle_graph(approvals, target + b' ', control, names)
            names["control_status"] = "not_approved"
            with self.assertRaisesRegex(ValueError, "lacks an approval"):
                policy.lifecycle_graph(approvals, target, control, names)

    def test_lifecycle_settings_retain_baseline_and_reviewed_provider_grants(self):
        graph = {"control_policies": [], "finalize_policies": [], "schema_documents": []}
        settings = policy.lifecycle_configuration("/private/journal.json", graph,
            {check.OWNER: ["verify_resource_control"], check.DISPLAY: [], legacy.ACTIONS: ["rebind_action_plan"]})
        self.assertIn("prepare", settings["operation_grants"][legacy.NVHTTP])
        self.assertIn("verify_resource_control", settings["operation_grants"][check.OWNER])
        self.assertEqual(settings["operation_grants"][check.DISPLAY], [])
        self.assertIn("authorize_action", settings["operation_grants"][legacy.ACTIONS])
        self.assertIn("rebind_action_plan", settings["operation_grants"][legacy.ACTIONS])
        self.assertIn(check.OWNER, settings["trusted_callers"])
        self.assertEqual(settings["consumer_requirements"]["capture_providers"], ["amaterasu.capture-pipewire/1"])
        with self.assertRaisesRegex(ValueError, "both providers"):
            policy.lifecycle_configuration("/private/journal.json", graph, {check.OWNER: []})


if __name__ == "__main__":
    unittest.main()
