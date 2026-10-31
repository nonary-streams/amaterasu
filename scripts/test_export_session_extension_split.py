#!/usr/bin/env python3
"""Checks for the review-only scoped settings split."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import export_session_extension_split as migration


class SplitTest(unittest.TestCase):
    def setUp(self):
        scopes = ["host", "application", "paired_client", "session"]
        def option(name, definition):
            return {"id": name, "definition": definition, "allowed_scopes": scopes}
        boolean = {"type": "boolean", "default": False}
        self.enabled = option("enabled", boolean)
        self.width = option("width", {"type": "integer", "default": 1920, "minimum": 1, "maximum": 4294967295})
        self.retention = option("retention", {"type": "enum", "default": "until_session_ends", "options": ["until_session_ends", "after_disconnect", "until_explicit_release"]})
        self.seconds = option("retention_seconds", {"type": "integer", "default": 300, "minimum": 0, "maximum": 4294967295})
        self.keepalive = option("keepalive_while_paused", boolean)
        self.enforce = option("enforce_while_paused", boolean)
        self.source = {"provider": migration.DISPLAY, "revision": 1, "options": [self.enabled, self.width, self.retention, self.seconds, self.keepalive, self.enforce]}
        self.display = {"provider": migration.DISPLAY, "revision": 2, "options": [self.enabled, self.width]}
        self.virtual = {"provider": migration.VIRTUAL, "revision": 1, "options": [self.enabled, self.retention, self.seconds, self.keepalive, self.enforce]}
        self.host = {"kind": "host"}
        self.app = {"kind": "application", "id": "game"}
        self.document = {"revision": 7, "schema_revision": 1, "layers": [
            {"scope": self.host, "values": {"enabled": True, "retention": "after_disconnect", "width": 1920}},
            {"scope": self.app, "values": {"enabled": False, "retention_seconds": 25}}]}
        self.choices = [{"scope": self.host, "enabled": False}, {"scope": self.app, "enabled": False}]

    def split(self):
        return migration.split(self.document, self.source, self.display, self.virtual, self.choices)

    def test_scopes_and_values_are_preserved_with_explicit_choices(self):
        result = self.split()
        self.assertEqual(result["display"]["layers"][0]["values"], {"enabled": False})
        self.assertEqual(result["virtual_screen"]["layers"][1]["values"], {"retention": "after_disconnect", "enabled": False})
        self.assertEqual(result["virtual_screen"]["layers"][0]["values"], {"retention_seconds": 25, "enabled": False})

    def test_missing_duplicate_and_invalid_choices_fail(self):
        self.choices.pop()
        with self.assertRaisesRegex(ValueError, "explicit enabled"):
            self.split()
        self.choices.append({"scope": self.host, "enabled": True})
        with self.assertRaisesRegex(ValueError, "duplicate enabled"):
            self.split()

    def test_unknown_values_and_incompatible_schemas_fail(self):
        self.document["layers"][0]["values"]["unknown"] = 1
        with self.assertRaisesRegex(ValueError, "unsupported setting"):
            self.split()
        del self.document["layers"][0]["values"]["unknown"]
        self.virtual = copy.deepcopy(self.virtual)
        self.virtual["options"][1]["definition"]["options"] = ["until_session_ends"]
        with self.assertRaisesRegex(ValueError, "incompatible destination"):
            self.split()

    def test_duplicate_scope_and_wrong_type_fail(self):
        self.document["layers"].append(copy.deepcopy(self.document["layers"][0]))
        with self.assertRaisesRegex(ValueError, "duplicate source scope"):
            self.split()
        self.document["layers"].pop()
        self.document["layers"][0]["values"]["width"] = True
        with self.assertRaisesRegex(ValueError, "invalid or unsupported"):
            self.split()

    def test_output_preserves_source_bytes_and_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            source_bytes = (json.dumps(self.document, indent=3) + "\n").encode()
            source = base / "source.json"
            source.write_bytes(source_bytes)
            paths = []
            for index, value in enumerate((self.source, self.display, self.virtual, self.choices)):
                path = base / f"input{index}.json"
                path.write_text(json.dumps(value))
                paths.append(path)
            # Stand-in publisher isolates this test from the binary build; production
            # descriptor validation is performed by amaterasu-release.
            publisher = base / "publisher"
            publisher.write_text("#!/bin/sh\nprintf '%064d\\n' 0\n")
            publisher.chmod(0o700)
            output = base / "output.json"
            args = [sys.executable, str(Path(migration.__file__)), str(publisher), str(source), *(str(p) for p in paths), str(output)]
            subprocess.run(args, check=True, capture_output=True)
            artifact = json.loads(output.read_text())
            self.assertEqual(bytes.fromhex(artifact["source_document_hex"]), source_bytes)
            self.assertEqual(artifact["source_sha256"], hashlib.sha256(source_bytes).hexdigest())
            self.assertEqual(source.read_bytes(), source_bytes)
            self.assertNotEqual(subprocess.run(args, capture_output=True).returncode, 0)
            self.assertEqual(source.read_bytes(), source_bytes)


if __name__ == "__main__":
    unittest.main()
