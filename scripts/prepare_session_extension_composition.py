#!/usr/bin/env python3
"""Prepare an opt-in two-provider Linux session package in new paths.

The v2 runtime gate currently refuses preparation until coordinator dispatch,
owner control/finalization and old-owner safety are implemented and qualified.
No activation, installation, service restart, or desktop operation is performed.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import tempfile

import check_session_extension_composition as check
from export_session_extension_split import load
import prepare_session_extension_profile as legacy
import session_extension_composition_policy as composition


def validated_selection(publisher: Path, repo: Path, library: Path, provider: str,
                        revision: int, methods: set[str], approved_methods_sha256: str) -> dict:
    report = check.check_selected(publisher, repo, library, provider, revision, methods)
    if report["approvals_sha256"] != approved_methods_sha256:
        raise ValueError(f"selected {provider} method approvals do not match the operator-approved digest")
    examples = repo / "examples"
    descriptor_bytes, descriptor = load(examples / "settings-descriptor.json")
    component_bytes, component = load(examples / "component-linux.json")
    approval_bytes, approvals = load(examples / "lifecycle-api-approvals.json")
    if (hashlib.sha256(component_bytes).hexdigest() != report["component_template_sha256"] or
            hashlib.sha256(approval_bytes).hexdigest() != report["approvals_sha256"]):
        raise ValueError("selected metadata changed during validation")
    # Recheck the same descriptor bytes against the typed digest. The component
    # builder later receives an immutable snapshot of all selected artifacts.
    with tempfile.TemporaryDirectory(prefix="amaterasu-descriptor-recheck-") as temporary:
        descriptor_path = Path(temporary) / "descriptor.json"
        descriptor_path.write_bytes(descriptor_bytes)
        if legacy.run(publisher, "extension-descriptor-digest", descriptor_path) != report["descriptor_sha256"]:
            raise ValueError("selected descriptor changed during validation")
    selected = {"descriptor": descriptor, "descriptor_digest": report["descriptor_sha256"],
                "component": component, "approvals": approvals["api_methods"]}
    return {"selected": selected, "report": report, "descriptor_bytes": descriptor_bytes,
            "component_bytes": component_bytes, "approval_bytes": approval_bytes,
            "library": library}


def validated_runtime_configuration() -> dict:
    # The current lifecycle runtime rejects composition v2. Do not let a caller
    # opt out of this gate or provide a self-asserted support flag.
    raise RuntimeError("split composition runtime and old-owner safety are not yet qualified")


def pack_profile(publisher: Path, root: Path, artifacts: Path, package: Path, sequence: int,
                 selections: list[dict], linux_options: list[str],
                 enable_action: bool = False) -> None:
    """Construct and verify a development package from already validated inputs.

    Invoked only after a release-controlled runtime and old-owner safety gate.
    The caller must validate lifecycle policy, selected method digests and all
    artifacts before reaching this function. Component snapshots and digest
    checks prevent packaging bytes different from those approved at preflight.
    """
    lifecycle_settings = validated_runtime_configuration()
    if root.exists() or root.is_symlink() or package.exists() or package.is_symlink():
        raise ValueError("root and package must be new paths")
    selected = [item["selected"] for item in selections]
    entries = composition.provider_entries(selected, enable_action)
    with tempfile.TemporaryDirectory(prefix="amaterasu-split-profile-") as temporary:
        directory = Path(temporary)
        snapshots = []
        for index, item in enumerate(selections):
            component = directory / f"component-{index}.json"
            library = directory / f"library-{index}"
            component.write_bytes(item["component_bytes"])
            # Read and bound the exact bytes, then compare to the preflight hash.
            with item["library"].open("rb") as source:
                artifact = source.read(256 * 1024 * 1024 + 1)
            if len(artifact) > 256 * 1024 * 1024 or hashlib.sha256(artifact).hexdigest() != item["report"]["artifact_sha256"]:
                raise ValueError("selected artifact changed during validation")
            library.write_bytes(artifact)
            snapshots.append((component, library))
        legacy.run(publisher, "init-linux-session", root, *linux_options)
        bootstrap_path = root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap, calls = composition.build_bootstrap(bootstrap, selected, entries, lifecycle_settings, enable_action)
        bootstrap_path.write_text(json.dumps(bootstrap, indent=2) + "\n")
        calls_path = directory / "calls.json"
        calls_path.write_text(json.dumps(calls))
        arguments = [publisher, "pack-linux-session-extensions", root, artifacts, package, sequence]
        for component, library in snapshots:
            arguments.extend(["--external-component", component, library])
        arguments.extend(["--call-grants", calls_path])
        legacy.run(*arguments)
        legacy.run(publisher, "verify", root, package)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--enable-display-actions", action="store_true")
    parser.add_argument("--approved-display-methods-sha256", required=True)
    parser.add_argument("--approved-virtual-methods-sha256", required=True)
    for name in ("publisher", "root", "artifacts", "package"):
        parser.add_argument(name, type=Path)
    parser.add_argument("sequence", type=int)
    for name in ("display_repo", "display_library", "virtual_repo", "virtual_library"):
        parser.add_argument(name, type=Path)
    parser.add_argument("linux_options", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.root.exists() or args.root.is_symlink() or args.package.exists() or args.package.is_symlink():
        parser.error("root and package must be new paths")
    check.check_schema(args.virtual_repo / "schemas/display-target-v1.json", check.TARGET_DIGEST)
    check.check_schema(args.virtual_repo / "schemas/display-control-v1.json", check.CONTROL_DIGEST)
    selected = [validated_selection(args.publisher, args.virtual_repo, args.virtual_library, check.OWNER,
                                    1, check.OWNER_METHODS, args.approved_virtual_methods_sha256),
                validated_selection(args.publisher, args.display_repo, args.display_library, check.DISPLAY,
                                    2, check.DISPLAY_METHODS, args.approved_display_methods_sha256)]
    entries = composition.provider_entries([item["selected"] for item in selected], args.enable_display_actions)
    del entries
    # This non-overridable gate runs before creating root or package paths.
    options = args.linux_options[1:] if args.linux_options[:1] == ["--"] else args.linux_options
    pack_profile(args.publisher, args.root, args.artifacts, args.package, args.sequence,
                 selected, options, args.enable_display_actions)
    print("Prepared and verified the signed split profile; it has not been activated.")


if __name__ == "__main__":
    main()
