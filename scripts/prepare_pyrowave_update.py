#!/usr/bin/env python3
"""Prepare a signed local PyroWave candidate without modifying the installed host."""
import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import shutil
import time

from local_bound_input import differences, policy_value
from local_full_update import CORE_REPOSITORY, compiled_from, core_package, core_signing, sign_core_bundle, source_state

REPLACEMENTS = {
    "amaterasu.encode": "libamaterasu_encode.so",
    "amaterasu.linux-session-video": "libamaterasu_video.so",
    "amaterasu.capture-pipewire": "libamaterasu_capture_pipewire.so",
    "amaterasu.media-transport": "libamaterasu_media_transport.so",
    "amaterasu.control-transport": "libamaterasu_control_transport.so",
    "amaterasu.stream-session": "libamaterasu_stream_session.so",
    "amaterasu.rtsp": "libamaterasu_rtsp.so",
    "amaterasu.nvhttp": "libamaterasu_nvhttp.so",
}
CONTROL = "amaterasu.encode.pyrowave/1"
PROVIDER = "amaterasu.encode.pyrowave-provider/1"
CONSUMER = "amaterasu.encode.pyrowave-consumer/1"
FRAMER = "amaterasu.encode.pyrowave-framing/1"
UNITS = "amaterasu.video.pyrowave-units/1"
TRANSPORT = "amaterasu.media-transport.pyrowave-plan/1"
ROUTER_COMPONENT = "amaterasu.router-mapping"
ROUTER_SERVICE = ROUTER_COMPONENT + "/1"
ROUTER_LIBRARY = "libamaterasu_upnp.so"
FRAME_FIT_SERVICE = "amaterasu.cpu-frame-fit/1"
SWSCALE_ABI_MARKER = b"AMATERASU_SWSCALE_ABI="
PROFILE = json.loads((Path(__file__).parent / "pyrowave/profile.json").read_text())
# 8-bit SDR and 10-bit HDR use different codec IDs. Chroma variants use
# the same wire selector and are distinguished by the client's ANNOUNCE.
SDR_PROFILE = deepcopy(PROFILE)
SDR_PROFILE["codec"]["value"] = 1
SDR_PROFILE["media_format"] = {"bit_depth": 8, "dynamic_range": "sdr", "chroma_sampling": "yuv420"}


def full_chroma(profile):
    variant = deepcopy(profile)
    variant["media_format"]["chroma_sampling"] = "yuv444"
    return variant


FULL_CHROMA_PROFILES = [full_chroma(PROFILE), full_chroma(SDR_PROFILE)]
PROFILES = [PROFILE, SDR_PROFILE, *FULL_CHROMA_PROFILES]
LEGACY_PROFILE = deepcopy(PROFILE)
LEGACY_PROFILE["wire_attributes"] = [attribute for attribute in PROFILE["wire_attributes"]
                                     if attribute["phase"] == "describe"]


def previous_chroma_composition(value):
    """Project the exact prior policy, including its original profile order."""
    if isinstance(value, dict):
        return {key: previous_chroma_composition(item) for key, item in value.items()
                if key != "transport_yuv444"}
    if isinstance(value, list):
        return [previous_chroma_composition(item) for item in value if item not in FULL_CHROMA_PROFILES]
    return value


def previous_profile_composition(value):
    """Recognize only the exact earlier profile, preserving other composition."""
    if value == PROFILE:
        return LEGACY_PROFILE
    if isinstance(value, dict):
        return {key: previous_profile_composition(item) for key, item in value.items()}
    if isinstance(value, list):
        return [previous_profile_composition(item) for item in value]
    return value



def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def compose(bootstrap, manifest, approved):
    candidate = deepcopy(bootstrap)
    policy = candidate["policy"]
    settings = policy["settings"]
    components = {item["id"]: item for item in manifest["components"]}
    if missing := REPLACEMENTS.keys() - components.keys():
        raise ValueError(f"installed package lacks required replacement components: {', '.join(sorted(missing))}")
    settings["amaterasu.encode"]["pyrowave"] = {
        "library_path": "/embedded/pyrowave", "gpu_only": True, "transport_enabled": True,
        "transport_yuv444": True,
    }
    settings["amaterasu.linux-session-video"].update({
        "unit_service": UNITS,
        "source_encoder_providers": {"pyrowave": {"control_service": CONTROL,
            "native_service": PROVIDER, "source_ids": [1], "extension_profiles": PROFILES, "submitted": True}},
        "external_encoder_consumers": {"pyrowave": CONSUMER},
    })
    settings["amaterasu.media-transport"].update({"plan_service": TRANSPORT, "extension_profiles": PROFILES})
    settings["amaterasu.stream-session"]["extension_route"] = {
        "profile": PROFILE, "alternate_profiles": PROFILES[1:], "source_id": 1, "submitted": True, "encoder_provider": "pyrowave",
        "encoder": {"clsid": "pyrowave", "friendly_name": "PyroWave", "hardware_url": None},
        "video_unit_service": UNITS, "framer_service": FRAMER, "plan_transport_service": TRANSPORT,
        "wire_budget_required": True,
    }
    settings["amaterasu.rtsp"]["extension_enabled"] = True
    settings["amaterasu.rtsp"]["extension_bit_depth"] = 10
    for principal, fields in {
        "amaterasu.encode": ["pyrowave"],
        "amaterasu.linux-session-video": ["unit_service", "source_encoder_providers", "external_encoder_consumers"],
        "amaterasu.media-transport": ["plan_service", "extension_profiles"],
        "amaterasu.stream-session": ["extension_route"],
        "amaterasu.rtsp": ["extension_enabled"],
    }.items():
        for field in fields:
            current = bootstrap["policy"]["settings"][principal]
            expected = settings[principal][field]
            before_chroma = previous_chroma_composition(expected)
            known_compositions = [
                expected, before_chroma, previous_profile_composition(expected),
                previous_profile_composition(before_chroma),
            ]
            if principal == "amaterasu.stream-session" and field == "extension_route":
                known_compositions.extend([
                    {key: value for key, value in item.items() if key != "wire_budget_required"}
                    for item in known_compositions
                ])
            if field in current and current[field] not in known_compositions:
                raise ValueError(f"existing extension composition requires reconciliation: {principal}")
    additions = {
        "amaterasu.encode": {"provide": [CONTROL, PROVIDER, CONSUMER, FRAMER], "native_provide": [PROVIDER, CONSUMER, FRAMER]},
        # Broker delegation intersects every ancestor's call authority. The
        # NVHTTP -> RTSP -> stream-session -> video probe must retain the
        # selected encoder service through the complete launch chain.
        "amaterasu.nvhttp": {"call": [CONTROL]},
        "amaterasu.rtsp": {"call": [CONTROL]},
        "amaterasu.linux-session-video": {"call": [CONTROL], "native_use": [PROVIDER, CONSUMER], "native_provide": [UNITS]},
        "amaterasu.media-transport": {"native_provide": [TRANSPORT]},
        "amaterasu.stream-session": {"call": [CONTROL], "native_use": [UNITS, FRAMER, TRANSPORT]},
    }
    video_settings = settings["amaterasu.linux-session-video"]
    capture_settings = settings["amaterasu.capture-pipewire"]
    if ("cpu_frame_fit_service" not in video_settings and "cpu_frame_fit_capture" not in video_settings
            and capture_settings.get("frame_fit_enabled") is not False):
        video_settings.update({"cpu_frame_fit_service": FRAME_FIT_SERVICE, "cpu_frame_fit_capture": "pipewire"})
    if video_settings.get("cpu_frame_fit_service") == FRAME_FIT_SERVICE:
        if capture_settings.setdefault("frame_fit_enabled", True) is not True:
            raise ValueError("selected PipeWire frame-fit service is disabled")
        additions["amaterasu.linux-session-video"]["native_use"].append(FRAME_FIT_SERVICE)
        additions["amaterasu.capture-pipewire"] = {"native_provide": [FRAME_FIT_SERVICE]}
        components["amaterasu.linux-session-video"]["requires"]["amaterasu.capture-pipewire"] = components["amaterasu.capture-pipewire"]["version"]
    for principal, grants in additions.items():
        for kind, services in grants.items():
            for target in (policy["principals"][principal], components[principal]["requested"]):
                target[kind] = sorted(set(target.get(kind, [])) | set(services))
    components["amaterasu.stream-session"]["requires"]["amaterasu.encode"] = components["amaterasu.encode"]["version"]
    if settings["amaterasu.nvhttp"].get("router_mapping_provider", ROUTER_SERVICE) == ROUTER_SERVICE:
        add_router_mapping(policy, components, manifest)
    allowlist = deepcopy(approved)
    for path, change in approved["changes"].items():
        if policy_value(bootstrap, path) != change["after"]:
            raise ValueError(f"existing approval is not applied: {path}")
    delta = differences(bootstrap, candidate)
    for path, change in delta.items():
        if not path.startswith("/policy/"):
            raise ValueError("cannot change bootstrap trust")
        if path in allowlist["changes"]:
            allowlist["changes"][path] = change
        else:
            ancestor = next((old for old in allowlist["changes"] if path.startswith(old + "/")), None)
            if ancestor is not None:
                # The existing approval covered the whole provider object.
                # Retire that obsolete transition and authorize only this
                # exact leaf upgrade from the currently approved endpoint.
                # The host verifies leaf changes, not overlapping ancestors.
                del allowlist["changes"][ancestor]
                allowlist["changes"][path] = change
                continue
            if any(old.startswith(path + "/") for old in allowlist["changes"]):
                raise ValueError(f"overlapping prior approval: {path}")
            allowlist["changes"][path] = change
    if len(allowlist["changes"]) > 256:
        raise ValueError("policy change limit exceeded")
    return candidate, allowlist, delta


def add_router_mapping(policy, components, manifest):
    nvhttp = "amaterasu.nvhttp"
    if ROUTER_COMPONENT not in components:
        runtime = components.get("amaterasu.runtime")
        default_network = "amaterasu.network/1"
        network_service = policy["settings"][nvhttp].get("network_provider", default_network)
        providers = [item for item in components.values()
                     if network_service in item["requested"].get("native_provide", [])
                     and network_service in policy["principals"].get(item["id"], {}).get("native_provide", [])]
        if runtime is None or len(providers) != 1:
            raise ValueError("router mapping requires the installed runtime and network provider")
        network = providers[0]
        grants = {"native_provide": [ROUTER_SERVICE], "native_use": [network_service],
                  "provide": [], "call": [], "publish": [], "subscribe": [], "hook": []}
        existing = policy["principals"].setdefault(ROUTER_COMPONENT, deepcopy(grants))
        if existing != grants:
            raise ValueError("existing router mapping grants require reconciliation")
        mapping_settings = policy["settings"].get(ROUTER_COMPONENT)
        if mapping_settings is not None and mapping_settings.get("network_provider", default_network) != network_service:
            raise ValueError("existing router mapping network requires reconciliation")
        if mapping_settings is None and network_service != default_network:
            policy["settings"][ROUTER_COMPONENT] = {"network_provider": network_service}
        component = {"id": ROUTER_COMPONENT, "version": "0.1.0", "file": ROUTER_LIBRARY,
                     "sha256": "", "size": 0, "abi_major": network["abi_major"], "runtime": False,
                     "requires": {item["id"]: item["version"] for item in (runtime, network)},
                     "requested": grants}
        manifest["components"].append(component)
        components[ROUTER_COMPONENT] = component
    components[nvhttp]["requires"][ROUTER_COMPONENT] = components[ROUTER_COMPONENT]["version"]
    for target in (policy["principals"][nvhttp], components[nvhttp]["requested"]):
        target["native_use"] = sorted(set(target.get("native_use", [])) | {ROUTER_SERVICE})


def prepare(root, tree, work):
    read = lambda path: json.loads(path.read_text())
    selection = read(root / "selection.json")
    if selection["pending"] or selection["trial"]:
        raise ValueError("another activation is in progress")
    bootstrap = read(root / "bootstrap.json")
    if bootstrap["development"] or bootstrap["allow_unsigned"]:
        raise ValueError("requires the existing signed production installation")
    active = root / "sets" / selection["active"]
    manifest = read(active / "manifest.json")
    approved = read(root / "package-activation-policy.json")
    candidate, allowlist, delta = compose(bootstrap, manifest, approved)
    signing = core_signing(root, bootstrap, approved)
    replacements = dict(REPLACEMENTS)
    if any(item["id"] == ROUTER_COMPONENT and not item["sha256"] for item in manifest["components"]):
        replacements[ROUTER_COMPONENT] = ROUTER_LIBRARY
    for file in replacements.values():
        artifact = tree / "target/release" / file
        if compiled_from(artifact) != tree:
            raise ValueError(f"artifact source mismatch: {file}")
    encode = tree / "target/release/libamaterasu_encode.so"
    if b"pyrowave_embedded_seal_failed" not in encode.read_bytes():
        raise ValueError("encoder does not contain the embedded PyroWave provider")
    video = tree / "target/release/libamaterasu_video.so"
    if b"submitted_extensions_worker_required" in video.read_bytes():
        raise ValueError("video was built without submitted extension support")
    if candidate["policy"]["settings"]["amaterasu.linux-session-video"].get("cpu_frame_fit_service") == FRAME_FIT_SERVICE:
        verify_frame_fit_backend(active, tree / "target/release", manifest)
    work.mkdir(mode=0o700)
    package = core_package(work, manifest)
    package.mkdir(parents=True, mode=0o700)
    replaced = []
    for component in manifest["components"]:
        previous = component["sha256"]
        source = tree / "target/release" / replacements[component["id"]] if component["id"] in replacements else active / component["file"]
        if component["id"] not in replacements and digest(source) != previous:
            raise ValueError(f"installed bytes changed: {component['id']}")
        shutil.copy2(source, package / component["file"])
        component["sha256"] = digest(source)
        component["size"] = source.stat().st_size
        if component["sha256"] != previous:
            replaced.append({"id": component["id"], "before": previous, "after": component["sha256"]})
    for component in manifest["components"]:
        component["requires"] = dict(sorted(component["requires"].items()))
    manifest["sequence"] = max(selection["highest_sequence"], int(time.time())) + 1
    if manifest["expires_unix"] <= time.time():
        raise ValueError("installed manifest expiry must be renewed explicitly")
    package_digest, catalog_digest = sign_core_bundle(package, manifest, signing)
    for name, value in [("candidate-bootstrap.json", candidate), ("package-activation-policy.json", allowlist),
                        ("policy-delta.json", delta)]:
        (work / name).write_text(json.dumps(value, indent=2) + "\n")
    metadata = {"root": str(root), "tree": str(tree), **source_state(tree),
        "previous": selection["active"], "digest": package_digest, "repository": CORE_REPOSITORY,
        "catalog_digest": catalog_digest, "selection_revision": digest(root / "selection.json"),
        "bootstrap_revision": digest(root / "bootstrap.json"),
        "activation_policy_revision": digest(root / "package-activation-policy.json"),
        "replaced": replaced, "unchanged_components": len(manifest["components"]) - len(replaced),
        "native_library_sha256": digest(tree / "native-deps/pyrowave/build/libpyrowave-shared.so.0.6.0")}
    (work / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps({"candidate": str(package), "digest": package_digest,
                      "replaced": len(replaced), "unchanged": metadata["unchanged_components"]}))


def verify_frame_fit_backend(active, artifacts, manifest):
    component_ids = {"amaterasu.linux-session-video", "amaterasu.capture-pipewire"}
    installed_conversion = any(SWSCALE_ABI_MARKER in (active / item["file"]).read_bytes()
                               for item in manifest["components"] if item["id"] in component_ids)
    pipewire = artifacts / REPLACEMENTS["amaterasu.capture-pipewire"]
    if installed_conversion and SWSCALE_ABI_MARKER not in pipewire.read_bytes():
        raise ValueError("installed FFmpeg frame fitting requires the PipeWire linux-ffmpeg-convert feature")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("work", type=Path)
    args = parser.parse_args()
    prepare(args.root.resolve(), Path(__file__).resolve().parents[1], args.work.resolve())
