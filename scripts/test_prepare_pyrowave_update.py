"""Local PyroWave preparation preserves trust and previously approved policy."""
from copy import deepcopy
from pathlib import Path
import tempfile
import unittest
from prepare_pyrowave_update import (compose, CONTROL, FRAME_FIT_SERVICE, FRAMER, PROVIDER,
                                     REPLACEMENTS, ROUTER_COMPONENT, ROUTER_SERVICE, UNITS,
                                     TRANSPORT, previous_profile_composition, verify_frame_fit_backend)


class CompositionTests(unittest.TestCase):
    def setUp(self):
        names = [*REPLACEMENTS, "amaterasu.runtime", "amaterasu.network"]
        self.bootstrap = {"development": False, "allow_unsigned": False,
            "trusted_keys": {"publisher": "key"}, "policy": {
                "principals": {name: {"provide": [name + "/1"], "call": ["existing/1"]} for name in names},
                "settings": {name: {"enabled": True} for name in names},
                "processing_compositions": {"existing": {"version": 1}}}}
        self.bootstrap["policy"]["principals"]["amaterasu.network"]["native_provide"] = ["amaterasu.network/1"]
        self.manifest = {"components": [{"id": name, "version": "0.1.0", "requires": {}, "abi_major": 1,
            "requested": deepcopy(self.bootstrap["policy"]["principals"][name])} for name in names]}
        self.approval = {"publisher_fingerprint": "publisher", "changes": {}}

    def test_update_requires_the_control_transport_with_semantic_framing(self):
        control = "amaterasu.control-transport"
        self.assertEqual(REPLACEMENTS[control], "libamaterasu_control_transport.so")
        self.manifest["components"] = [item for item in self.manifest["components"] if item["id"] != control]
        with self.assertRaisesRegex(ValueError, r"lacks required replacement components: amaterasu\.control-transport"):
            compose(self.bootstrap, self.manifest, self.approval)

    def test_update_installs_router_mapping_and_authorizes_frame_fit(self):
        policy, _, _ = compose(self.bootstrap, self.manifest, self.approval)
        components = {item["id"]: item for item in self.manifest["components"]}
        grants = policy["policy"]["principals"]
        self.assertEqual(list(components[ROUTER_COMPONENT]),
                         ["id", "version", "file", "sha256", "size", "abi_major", "runtime", "requires", "requested"])
        self.assertEqual(list(components[ROUTER_COMPONENT]["requested"]),
                         ["native_provide", "native_use", "provide", "call", "publish", "subscribe", "hook"])
        self.assertEqual(components[ROUTER_COMPONENT]["requires"],
                         {"amaterasu.runtime": "0.1.0", "amaterasu.network": "0.1.0"})
        self.assertEqual(components["amaterasu.nvhttp"]["requires"][ROUTER_COMPONENT], "0.1.0")
        self.assertIn(ROUTER_SERVICE, grants["amaterasu.nvhttp"]["native_use"])
        self.assertIn(FRAME_FIT_SERVICE, grants["amaterasu.capture-pipewire"]["native_provide"])
        self.assertIn(FRAME_FIT_SERVICE, grants["amaterasu.linux-session-video"]["native_use"])
        self.assertIs(policy["policy"]["settings"]["amaterasu.capture-pipewire"]["frame_fit_enabled"], True)

    def test_update_preserves_disabled_router_and_custom_frame_fit(self):
        settings = self.bootstrap["policy"]["settings"]
        settings["amaterasu.nvhttp"]["router_mapping_provider"] = None
        settings["amaterasu.linux-session-video"].update(
            {"cpu_frame_fit_service": "example.fit/1", "cpu_frame_fit_capture": "example"})
        policy, _, _ = compose(self.bootstrap, self.manifest, self.approval)
        self.assertNotIn(ROUTER_COMPONENT, {item["id"] for item in self.manifest["components"]})
        self.assertEqual(policy["policy"]["settings"]["amaterasu.linux-session-video"]["cpu_frame_fit_service"],
                         "example.fit/1")

    def test_router_inherits_installed_nvhttp_network_selection(self):
        policy = self.bootstrap["policy"]
        policy["settings"]["amaterasu.nvhttp"]["network_provider"] = "example.network/1"
        policy["principals"]["example.network"] = {"native_provide": ["example.network/1"]}
        self.manifest["components"].append({"id": "example.network", "version": "2.0.0", "abi_major": 1,
                                             "requested": deepcopy(policy["principals"]["example.network"])})
        candidate, _, _ = compose(self.bootstrap, self.manifest, self.approval)
        router = next(item for item in self.manifest["components"] if item["id"] == ROUTER_COMPONENT)
        self.assertEqual(router["requires"]["example.network"], "2.0.0")
        self.assertEqual(router["requested"]["native_use"], ["example.network/1"])
        self.assertEqual(candidate["policy"]["settings"][ROUTER_COMPONENT]["network_provider"], "example.network/1")

    def test_update_preserves_opted_in_ffmpeg_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            active, artifacts = (Path(directory) / name for name in ("active", "artifacts"))
            active.mkdir()
            artifacts.mkdir()
            video = "amaterasu.linux-session-video"
            pipewire = REPLACEMENTS["amaterasu.capture-pipewire"]
            manifest = {"components": [{"id": video, "file": REPLACEMENTS[video]}]}
            (active / REPLACEMENTS[video]).write_bytes(b"AMATERASU_SWSCALE_ABI=9,60\0")
            (artifacts / pipewire).write_bytes(b"no FFmpeg support")
            with self.assertRaisesRegex(ValueError, "requires the PipeWire linux-ffmpeg-convert feature"):
                verify_frame_fit_backend(active, artifacts, manifest)
            (artifacts / pipewire).write_bytes(b"AMATERASU_SWSCALE_ABI=9,60\0")
            verify_frame_fit_backend(active, artifacts, manifest)

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

    def test_launch_chain_retains_selected_encoder_authority_at_every_hop(self):
        candidate, _, _ = compose(self.bootstrap, self.manifest, self.approval)
        grants = candidate["policy"]["principals"]
        components = {item["id"]: item for item in self.manifest["components"]}
        delegated = None
        for name in ["amaterasu.nvhttp", "amaterasu.rtsp", "amaterasu.stream-session",
                     "amaterasu.linux-session-video"]:
            effective = set(grants[name]["call"]) & set(components[name]["requested"]["call"])
            delegated = effective if delegated is None else delegated & effective
            self.assertIn(CONTROL, delegated, name)
            self.assertIn("existing/1", effective, name)

    def test_existing_extension_update_keeps_authority_approval_source(self):
        first, approval, _ = compose(self.bootstrap, self.manifest, self.approval)
        second, updated, delta = compose(first, self.manifest, approval)
        self.assertEqual(first, second)
        self.assertEqual(approval, updated)
        self.assertEqual(delta, {})

    def test_new_destination_starts_at_the_currently_approved_endpoint(self):
        path = '/policy/principals/amaterasu.encode/provide'
        self.approval['changes'][path] = {'before': {'kind': 'absent'},
            'after': {'kind': 'present', 'value': ['amaterasu.encode/1']}}
        _, approval, _ = compose(self.bootstrap, self.manifest, self.approval)
        self.assertEqual(approval['changes'][path]['before'], self.approval['changes'][path]['after'])
        self.assertIn(FRAMER, approval['changes'][path]['after']['value'])

    def test_exact_earlier_profile_migrates_without_accepting_other_compositions(self):
        first, approval, _ = compose(self.bootstrap, self.manifest, self.approval)
        old = previous_profile_composition(first)
        old_approval = deepcopy(approval)
        for path, change in old_approval["changes"].items():
            change["after"] = previous_profile_composition(change["after"])
        candidate, updated, delta = compose(old, self.manifest, old_approval)
        self.assertEqual(candidate, first)
        self.assertTrue(delta)
        # Exact endpoint checks used by the installed activation guard must
        # accept the current policy before a trial can be prepared.
        from local_bound_input import policy_value
        for path, change in updated['changes'].items():
            self.assertIn(policy_value(old, path), (change['before'], change['after']))
            self.assertEqual(policy_value(candidate, path), change['after'])
        old["policy"]["settings"]["amaterasu.stream-session"]["extension_route"]["source_id"] = 2
        with self.assertRaisesRegex(ValueError, "requires reconciliation"):
            compose(old, self.manifest, old_approval)

    def test_existing_route_without_wire_budget_probe_migrates(self):
        first, approval, _ = compose(self.bootstrap, self.manifest, self.approval)
        old = deepcopy(first)
        route_path = "/policy/settings/amaterasu.stream-session/extension_route"
        old_route = old["policy"]["settings"]["amaterasu.stream-session"]["extension_route"]
        old_route.pop("wire_budget_required")
        approval["changes"][route_path]["after"]["value"] = deepcopy(old_route)
        candidate, updated, _ = compose(old, self.manifest, approval)
        self.assertEqual(first, candidate)
        self.assertEqual(updated["changes"][route_path + "/wire_budget_required"], {
            "before": {"kind": "absent"}, "after": {"kind": "present", "value": True},
        })

    def test_unapplied_approval_is_rejected(self):
        self.approval['changes']['/policy/settings/pending'] = {
            'before': {'kind': 'absent'}, 'after': {'kind': 'present', 'value': True}}
        with self.assertRaisesRegex(ValueError, 'not applied'):
            compose(self.bootstrap, self.manifest, self.approval)


if __name__ == '__main__':
    unittest.main()
