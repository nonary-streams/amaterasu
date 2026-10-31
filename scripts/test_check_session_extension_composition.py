#!/usr/bin/env python3
"""Preflight checks for selected v2 provider contracts."""
import copy
import hashlib
from pathlib import Path
import tempfile
import unittest

import check_session_extension_composition as check


class CompositionPreflightTest(unittest.TestCase):
    def setUp(self):
        self.provider = check.OWNER
        service = self.provider + "/2"
        self.descriptor = {"provider": self.provider, "revision": 1,
                           "settings_validate": {"service": service, "method": "settings_validate",
                                                 "interface_major": 2, "interface_minor": 0, "schema_revision": 1},
                           "options": [{"id": "enabled", "definition": {"type": "boolean", "default": False}}]}
        self.component = {"id": self.provider, "runtime": False, "requires": {"amaterasu.runtime": "0.1.0"},
                          "requested": {"provide": [service], "call": []}}
        self.approvals = [{"provider": self.provider, "method": {"service": service, "method": name,
                          "interface_major": 2, "interface_minor": 0}} for name in sorted(check.OWNER_METHODS)]

    def check(self):
        check.check_provider(self.descriptor, self.component, self.approvals,
                             self.provider, 1, check.OWNER_METHODS)

    def test_selected_v2_contract(self):
        self.check()

    def test_wrong_service_and_enabled_default_fail(self):
        self.component["requested"]["provide"] = [self.provider + "/1"]
        with self.assertRaisesRegex(ValueError, "selected v2"):
            self.check()
        self.component["requested"]["provide"] = [self.provider + "/2"]
        self.descriptor["options"][0]["definition"]["default"] = True
        with self.assertRaisesRegex(ValueError, "start disabled"):
            self.check()

    def test_missing_extra_and_duplicate_approval_fail(self):
        removed = self.approvals.pop()
        with self.assertRaisesRegex(ValueError, "incomplete"):
            self.check()
        self.approvals.append(removed)
        self.approvals[-1] = copy.deepcopy(self.approvals[0])
        with self.assertRaisesRegex(ValueError, "duplicated"):
            self.check()
        self.approvals[-1]["method"]["method"] = "unexpected"
        with self.assertRaisesRegex(ValueError, "unexpected"):
            self.check()

    def test_wrong_version_and_provider_fail(self):
        self.approvals[0]["method"]["interface_major"] = 1
        with self.assertRaisesRegex(ValueError, "select v2"):
            self.check()
        self.approvals[0]["method"]["interface_major"] = 2
        self.approvals[0]["provider"] = check.DISPLAY
        with self.assertRaisesRegex(ValueError, "provider mismatch"):
            self.check()

    def test_schema_digest_checks_exact_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            schema = Path(temporary) / "schema.json"
            schema.write_bytes(b"{}\n")
            digest = hashlib.sha256(b"{}\n").hexdigest()
            self.assertEqual(check.check_schema(schema, digest), digest)
            schema.write_bytes(b"{}")
            with self.assertRaisesRegex(ValueError, "unexpected schema bytes"):
                check.check_schema(schema, digest)


if __name__ == "__main__":
    unittest.main()
