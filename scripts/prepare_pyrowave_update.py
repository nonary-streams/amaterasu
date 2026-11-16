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
from local_full_update import compact, source_state, compiled_from

REPLACEMENTS = {
    "amaterasu.encode": "libamaterasu_encode.so",
    "amaterasu.linux-session-video": "libamaterasu_video.so",
    "amaterasu.media-transport": "libamaterasu_media_transport.so",
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
PROFILE = json.loads((Path(__file__).parent / "pyrowave/profile.json").read_text())
# The same extension for SDR clients: Moonlight requests 8-bit 4:2:0 PyroWave
# whenever HDR is off, and the encoder's 8-bit codec serves it.
SDR_PROFILE = deepcopy(PROFILE)
SDR_PROFILE["codec"]["value"] = 1
SDR_PROFILE["media_format"] = {"bit_depth": 8, "dynamic_range": "sdr", "chroma_sampling": "yuv420"}
LEGACY_PROFILE = deepcopy(PROFILE)
LEGACY_PROFILE["wire_attributes"] = [attribute for attribute in PROFILE["wire_attributes"]
                                     if attribute["phase"] == "describe"]


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
    settings["amaterasu.encode"]["pyrowave"] = {
        "library_path": "/embedded/pyrowave", "gpu_only": True, "transport_enabled": True,
    }
    settings["amaterasu.linux-session-video"].update({
        "unit_service": UNITS,
        "source_encoder_providers": {"pyrowave": {"control_service": CONTROL,
            "native_service": PROVIDER, "source_ids": [1], "extension_profiles": [PROFILE, SDR_PROFILE], "submitted": True}},
        "external_encoder_consumers": {"pyrowave": CONSUMER},
    })
    settings["amaterasu.media-transport"].update({"plan_service": TRANSPORT, "extension_profiles": [PROFILE, SDR_PROFILE]})
    settings["amaterasu.stream-session"]["extension_route"] = {
        "profile": PROFILE, "alternate_profiles": [SDR_PROFILE], "source_id": 1, "submitted": True, "encoder_provider": "pyrowave",
        "encoder": {"clsid": "pyrowave", "friendly_name": "PyroWave", "hardware_url": None},
        "video_unit_service": UNITS, "framer_service": FRAMER, "plan_transport_service": TRANSPORT,
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
            if field in current and current[field] not in (expected, previous_profile_composition(expected)):
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
    components = {item["id"]: item for item in manifest["components"]}
    for principal, grants in additions.items():
        for kind, services in grants.items():
            for target in (policy["principals"][principal], components[principal]["requested"]):
                target[kind] = sorted(set(target.get(kind, [])) | set(services))
    components["amaterasu.stream-session"]["requires"]["amaterasu.encode"] = components["amaterasu.encode"]["version"]
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


def prepare(root, tree, work):
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
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
    repository = "amaterasu-rtx-hdr"
    approval = read(root / "repositories/approved.json")["repositories"][repository]
    fingerprint = approval["approval"]["fingerprint"]
    key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(read(root / "development-key.json")))
    public = key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    if hashlib.sha256(public).hexdigest() != fingerprint or approved["publisher_fingerprint"] != fingerprint:
        raise ValueError("local key is not the existing approved publisher")
    for file in REPLACEMENTS.values():
        artifact = tree / "target/release" / file
        if compiled_from(artifact) != tree:
            raise ValueError(f"artifact source mismatch: {file}")
    encode = tree / "target/release/libamaterasu_encode.so"
    if b"pyrowave_embedded_seal_failed" not in encode.read_bytes():
        raise ValueError("encoder does not contain the embedded PyroWave provider")
    video = tree / "target/release/libamaterasu_video.so"
    if b"submitted_extensions_worker_required" in video.read_bytes():
        raise ValueError("video was built without submitted extension support")
    work.mkdir(mode=0o700)
    package = work / "bundle/native/amaterasu-linux-session-rtx-hdr/0.1.0"
    package.mkdir(parents=True, mode=0o700)
    replaced = []
    for component in manifest["components"]:
        previous = component["sha256"]
        source = tree / "target/release" / REPLACEMENTS[component["id"]] if component["id"] in REPLACEMENTS else active / component["file"]
        if component["id"] not in REPLACEMENTS and digest(source) != previous:
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
    (package / "manifest.json").write_bytes(compact(manifest))
    blob = (package / "manifest.json").read_bytes()
    (package / "manifest.sig").write_bytes(compact({"key_id": fingerprint, "signature": key.sign(blob).hex()}))
    package_digest = digest(package / "manifest.json")
    catalog = compact({"schema": 1, "repository": repository,
        "sequence": max(approval["checkpoint"]["highest_sequence"], int(time.time())) + 1,
        "expires_unix": manifest["expires_unix"], "target": manifest["target"],
        "candidates": [{"slot": "amaterasu.rtx-hdr", "implementation": "amaterasu-linux-session-rtx-hdr",
            "version": "0.1.0", "kind": "native_set", "manifest_sha256": package_digest}]})
    (work / "bundle/catalog.json").write_bytes(catalog)
    (work / "bundle/catalog.sig").write_text(key.sign(b"amaterasu.repository.catalog.v1\n" + catalog).hex())
    for name, value in [("candidate-bootstrap.json", candidate), ("package-activation-policy.json", allowlist),
                        ("policy-delta.json", delta)]:
        (work / name).write_text(json.dumps(value, indent=2) + "\n")
    metadata = {"root": str(root), "tree": str(tree), **source_state(tree),
        "previous": selection["active"], "digest": package_digest, "repository": repository,
        "catalog_digest": digest(work / "bundle/catalog.json"), "selection_revision": digest(root / "selection.json"),
        "bootstrap_revision": digest(root / "bootstrap.json"),
        "activation_policy_revision": digest(root / "package-activation-policy.json"),
        "replaced": replaced, "unchanged_components": len(manifest["components"]) - len(replaced),
        "native_library_sha256": digest(tree / "native-deps/pyrowave/build/libpyrowave-shared.so.0.6.0")}
    (work / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    print(json.dumps({"candidate": str(package), "digest": package_digest,
                      "replaced": len(replaced), "unchanged": metadata["unchanged_components"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("work", type=Path)
    args = parser.parse_args()
    prepare(args.root.resolve(), Path(__file__).resolve().parents[1], args.work.resolve())
