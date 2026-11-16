"""Local PyroWave preparation preserves trust and previously approved policy."""
from copy import deepcopy
import unittest
from prepare_pyrowave_update import compose, FRAMER, PROVIDER, UNITS, TRANSPORT


class CompositionTests(unittest.TestCase):
    def setUp(self):
        names = ["amaterasu.encode", "amaterasu.linux-session-video", "amaterasu.media-transport",
                 "amaterasu.stream-session", "amaterasu.rtsp"]
        self.bootstrap = {"development": False, "allow_unsigned": False,
            "trusted_keys": {"publisher": "key"}, "policy": {
                "principals": {name: {"provide": [name + "/1"], "call": ["existing/1"]} for name in names},
                "settings": {name: {"enabled": True} for name in names},
                "processing_compositions": {"existing": {"version": 1}}}}
        self.manifest = {"components": [{"id": name, "version": "0.1.0", "requires": {},
            "requested": deepcopy(self.bootstrap["policy"]["principals"][name])} for name in names]}
        self.approval = {"publisher_fingerprint": "publisher", "changes": {}}

    def test_grants_cover_the_registered_services_and_preserve_trust(self):
        before = deepcopy(self.bootstrap)
        candidate, approval, delta = compose(self.bootstrap, self.manifest, self.approval)
        self.assertEqual(before, self.bootstrap)
        self.assertEqual(candidate["trusted_keys"], before["trusted_keys"])
        self.assertEqual(candidate["policy"]["processing_compositions"], before["policy"]["processing_compositions"])
        grants = candidate["policy"]["principals"]
        self.assertIn(FRAMER, grants["amaterasu.encode"]["provide"])
        self.assertIn(FRAMER, grants["amaterasu.encode"]["native_provide"])
        self.assertIn(PROVIDER, grants["amaterasu.linux-session-video"]["native_use"])
        self.assertEqual(set(grants["amaterasu.stream-session"]["native_use"]), {UNITS, FRAMER, TRANSPORT})
        self.assertTrue(all(path.startswith('/policy/') for path in delta))
        self.assertEqual(approval['publisher_fingerprint'], 'publisher')

    def test_already_applied_overlapping_approval_keeps_its_original_source(self):
        path = '/policy/principals/amaterasu.encode/provide'
        self.approval['changes'][path] = {'before': {'kind': 'absent'},
            'after': {'kind': 'present', 'value': ['amaterasu.encode/1']}}
        _, approval, _ = compose(self.bootstrap, self.manifest, self.approval)
        self.assertEqual(approval['changes'][path]['before'], {'kind': 'absent'})
        self.assertIn(FRAMER, approval['changes'][path]['after']['value'])

    def test_unapplied_approval_is_rejected(self):
        self.approval['changes']['/policy/settings/pending'] = {
            'before': {'kind': 'absent'}, 'after': {'kind': 'present', 'value': True}}
        with self.assertRaisesRegex(ValueError, 'not applied'):
            compose(self.bootstrap, self.manifest, self.approval)


if __name__ == '__main__':
    unittest.main()
