import fcntl
import json
from pathlib import Path
import tempfile
import unittest

from withdraw_linux_extension import withdraw


class WithdrawalTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.path = self.root / 'bootstrap.json'
        self.data = {'development': True, 'policy': {'settings': {
            'amaterasu.management': {'extensions': {'entries': [
                {'id': 'alpha', 'root': '/var/tmp/beta', 'trusted_keys': {'abc': 'value'}},
                {'id': 'beta', 'root': '/var/tmp/alpha', 'trusted_keys': {}}]}},
            'amaterasu.authentication': {'authorization_policy_generation': 9}}}}
        self.path.write_text(json.dumps(self.data))
        self.path.chmod(0o600)

    def tearDown(self):
        self.tmp.cleanup()

    def test_remove_exact_entry_and_revoke_generation(self):
        self.assertEqual(withdraw(self.root, 'alpha'), 10)
        result = json.loads(self.path.read_text())['policy']['settings']
        self.assertEqual(result['amaterasu.authentication']['authorization_policy_generation'], 10)
        self.assertEqual(result['amaterasu.management']['extensions']['entries'], [self.data['policy']['settings']['amaterasu.management']['extensions']['entries'][1]])
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_failure_preserves_bootstrap(self):
        for name in ('missing', '../alpha'):
            before = self.path.read_bytes()
            with self.assertRaises(ValueError):
                withdraw(self.root, name)
            self.assertEqual(self.path.read_bytes(), before)
        self.data['policy']['settings']['amaterasu.authentication']['authorization_policy_generation'] = 2**64 - 1
        self.path.write_text(json.dumps(self.data))
        before = self.path.read_bytes()
        with self.assertRaises(ValueError):
            withdraw(self.root, 'alpha')
        self.assertEqual(self.path.read_bytes(), before)

    def test_duplicate_and_missing_ids_are_rejected_without_writes(self):
        entries = self.data['policy']['settings']['amaterasu.management']['extensions']['entries']
        for id_value in ('alpha', None):
            if id_value is None:
                entries[1].pop('id', None)
            else:
                entries[1]['id'] = id_value
            self.path.write_text(json.dumps(self.data))
            before = self.path.read_bytes()
            with self.assertRaises(ValueError):
                withdraw(self.root, 'alpha')
            self.assertEqual(self.path.read_bytes(), before)

    def test_busy_root_is_unchanged(self):
        before = self.path.read_bytes()
        with (self.root / 'host.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaises(BlockingIOError):
                withdraw(self.root, 'alpha')
        self.assertEqual(self.path.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
