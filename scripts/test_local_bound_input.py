#!/usr/bin/env python3
"""External-input updates preserve host policy and reject foreign composition."""
from copy import deepcopy
import unittest

from local_bound_input import COORDINATOR, INPUT, STREAM, policy_value, prepare_bound_input


class BoundInputTests(unittest.TestCase):
    def setUp(self):
        self.bootstrap = {
            "development": False, "allow_unsigned": False, "trusted_keys": {"existing": "key"},
            "policy": {"principals": {p: {"provide": [p + '/1'], "call": ['existing/1']} for p in [INPUT, COORDINATOR, STREAM]},
                       "settings": {COORDINATOR: {"capture_binding_mode": "external", "consumers": {"capture": [STREAM]},
                                                  "operation_grants": {STREAM: ['existing']}, "trusted_callers": [STREAM],
                                                  "participants": [{"provider": "independent-plugin"}]}}}}
        self.manifest = {"components": [{"id": p, "requested": deepcopy(g)} for p, g in self.bootstrap['policy']['principals'].items()]}
        self.allowlist = {"publisher_fingerprint": 'trusted', "changes": {}}

    def test_preparation_returns_exact_changes_without_modifying_the_host_policy(self):
        before = deepcopy(self.bootstrap)
        result = prepare_bound_input(self.bootstrap, self.manifest, self.allowlist)
        self.assertEqual(before, self.bootstrap)
        self.assertEqual(result['publisher_fingerprint'], 'trusted')
        for path, change in result['changes'].items():
            self.assertTrue(path.startswith('/policy/'))
            self.assertEqual(change['before'], policy_value(before, path))
        self.assertIn('/policy/settings/' + COORDINATOR + '/input_provider', result['changes'])
        calls = self.manifest['components'][0]['requested']['call']
        self.assertEqual(calls, [COORDINATOR + '/1', 'existing/1'])

    def test_existing_approved_destinations_are_preserved(self):
        path = '/policy/settings/' + COORDINATOR + '/participants'
        change = {"before": {"kind": "absent"}, "after": policy_value(self.bootstrap, path)}
        self.allowlist['changes'][path] = change
        result = prepare_bound_input(self.bootstrap, self.manifest, self.allowlist)
        self.assertEqual(result['changes'][path], change)

    def test_pending_approvals_are_not_overwritten(self):
        self.allowlist['changes']['/policy/missing'] = {"before": {"kind": "absent"}, "after": {"kind": "present", "value": True}}
        with self.assertRaisesRegex(ValueError, 'not fully applied'):
            prepare_bound_input(self.bootstrap, self.manifest, self.allowlist)

    def test_independent_input_providers_are_preserved(self):
        self.bootstrap['policy']['settings'][COORDINATOR]['input_provider'] = 'replacement-input'
        before = deepcopy(self.manifest)
        with self.assertRaisesRegex(ValueError, 'another input provider'):
            prepare_bound_input(self.bootstrap, self.manifest, self.allowlist)
        self.assertEqual(before, self.manifest)

    def test_managed_capture_is_not_migrated(self):
        self.bootstrap['policy']['settings'][COORDINATOR]['capture_binding_mode'] = 'managed'
        with self.assertRaisesRegex(ValueError, 'requires external capture'):
            prepare_bound_input(self.bootstrap, self.manifest, self.allowlist)


if __name__ == '__main__':
    unittest.main()
