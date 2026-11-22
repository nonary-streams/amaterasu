#!/usr/bin/env python3
"""Prepare, activate and audit local updates built from one source tree.

Replacing only some components mixes builds from different revisions. Interfaces
between components are not always versioned when they change, so a mixed set can
load cleanly and then fail while streaming. This tool therefore packages only a
complete set whose every library was compiled from the named tree, and records
what it installed so that `check` can report any later drift.

    local_full_update.py prepare ROOT TREE WORK HOST_BINARY
    local_full_update.py activate ROOT WORK
    local_full_update.py adopt-host ROOT WORK
    local_full_update.py check ROOT
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

SERVICE = "amaterasu-linux-session.service"
UNIT = Path.home() / ".config/systemd/user" / SERVICE
PROVENANCE = "install-provenance.json"
HOST_INSTALL_PARENT = Path.home() / ".local/share/amaterasu"
CORE_REPOSITORY = "amaterasu"
CORE_SLOT = "amaterasu.runtime"
CORE_IMPLEMENTATION = "amaterasu-core"
CATALOG_DOMAIN = b"amaterasu.repository.catalog.v1\n"
COMMIT_TIMEOUT_S = 120
SEPARATELY_BUILT = {
    "amaterasu.capture-vkms": Path("modules/capture-vkms/capture/target/release"),
}


def read_json(path: Path):
    return json.loads(path.read_text())


def compact(value) -> bytes:
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def core_signing(root: Path, bootstrap: dict, allowlist: dict):
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
    from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

    repositories = read_json(root / "repositories/approved.json")["repositories"]
    approval = repositories.get(CORE_REPOSITORY)
    if approval is None or approval["revoked"]:
        raise ValueError("enroll the Amaterasu Core repository before preparing an update")
    identity = approval["approval"]
    fingerprint = identity["fingerprint"]
    if (allowlist["publisher_fingerprint"] != fingerprint
            or bootstrap["trusted_keys"].get(fingerprint) != identity["public_key"]):
        raise ValueError("the host has not authorized the Core publisher for native activation")
    encoded = os.environ.get("AMATERASU_PUBLISHER_KEY")
    if encoded is None:
        key_file = root / "development-key.json"
        encoded = read_json(key_file) if key_file.is_file() else ""
    key_bytes = bytes.fromhex(encoded.strip())
    if len(key_bytes) != 32:
        raise ValueError("provide the existing Core signing key through AMATERASU_PUBLISHER_KEY")
    key = Ed25519PrivateKey.from_private_bytes(key_bytes)
    public_key = key.public_key()
    public = public_key.public_bytes(Encoding.Raw, PublicFormat.Raw)
    if hashlib.sha256(public).hexdigest() != fingerprint:
        raise ValueError("the signing key does not match the Core publisher")
    checkpoint = approval["checkpoint"]["highest_sequence"]
    saved = approval.get("catalog")
    offers = []
    if saved is not None:
        raw = saved["raw"].encode()
        public_key.verify(bytes.fromhex(saved["signature"]), CATALOG_DOMAIN + raw)
        catalog = json.loads(raw)
        if catalog["repository"] != CORE_REPOSITORY or catalog["sequence"] != checkpoint:
            raise ValueError("the cached Core catalog differs from its approval")
        offers = catalog["candidates"]
    return key, fingerprint, checkpoint, offers


def core_package(work: Path, manifest: dict) -> Path:
    version = next(component["version"] for component in manifest["components"]
                   if component["id"] == CORE_SLOT)
    if version in (".", "..") or not re.fullmatch(r"[a-z0-9._-]{1,128}", version):
        raise ValueError("the runtime version cannot be used in a repository path")
    return work / "bundle/native" / CORE_IMPLEMENTATION / version


def sign_core_bundle(package: Path, manifest: dict, signing: tuple) -> tuple[str, str]:
    key, fingerprint, checkpoint, offers = signing
    blob = compact(manifest)
    digest = hashlib.sha256(blob).hexdigest()
    (package / "manifest.json").write_bytes(blob)
    (package / "manifest.sig").write_bytes(
        compact({"key_id": fingerprint, "signature": key.sign(blob).hex()}))
    candidate = {
        "slot": CORE_SLOT, "implementation": CORE_IMPLEMENTATION,
        "version": package.name, "kind": "native_set", "manifest_sha256": digest,
    }
    # Installed plugins still use this catalog to verify their original offer.
    identity = (CORE_SLOT, CORE_IMPLEMENTATION, package.name)
    retained = [offer for offer in offers
                if (offer["slot"], offer["implementation"], offer["version"]) != identity]
    catalog = compact({
        "schema": 1,
        "repository": CORE_REPOSITORY,
        "sequence": max(checkpoint, int(time.time())) + 1,
        "expires_unix": manifest["expires_unix"],
        "target": manifest["target"],
        "candidates": [candidate, *retained],
    })
    bundle = package.parents[2]
    (bundle / "catalog.json").write_bytes(catalog)
    (bundle / "catalog.sig").write_text(key.sign(CATALOG_DOMAIN + catalog).hex())
    return digest, hashlib.sha256(catalog).hexdigest()


def git(tree: Path, *arguments: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(tree), *arguments])


def compiled_from(artifact: Path) -> Path | None:
    """The source tree recorded in the compiler's dependency file."""
    dependency_file = artifact.with_suffix(".d")
    if not dependency_file.is_file():
        return None
    _, _, sources = dependency_file.read_text().partition(":")
    match = re.search(r"(/\S*?)/(?:modules|crates)/", sources)
    return Path(match.group(1)) if match else None


def artifact_path(tree: Path, component: dict, target: Path | None = None) -> Path:
    if component["id"] in SEPARATELY_BUILT:
        return tree / SEPARATELY_BUILT[component["id"]] / component["file"]
    return (target or tree / "target") / "release" / component["file"]


def foreign_artifacts(tree: Path, components: list[dict], target: Path | None = None) -> list[str]:
    """Every component that is missing or was compiled from another tree."""
    problems = []
    for component in components:
        artifact = artifact_path(tree, component, target)
        if not artifact.is_file():
            problems.append(f"{component['id']}: {artifact} is missing")
            continue
        source = compiled_from(artifact)
        if source != tree:
            problems.append(f"{component['id']}: compiled from {source}, not {tree}")
    return problems


def missing_backend_features(bootstrap: dict, manifest: dict, artifacts: dict[str, Path]) -> list[str]:
    """An enabled Linux display provider needs its compiled native backend."""
    if "linux" not in manifest["target"]:
        return []
    identifier = "amaterasu.virtual-display"
    settings = bootstrap["policy"].get("settings", {}).get(identifier, {})
    if not settings.get("enabled", False):
        return []
    artifact = artifacts.get(identifier)
    if artifact is None or not artifact.is_file():
        return [f"{identifier}: enabled provider artifact is missing"]
    if b"AMATERASU_LINUX_VDD=" not in artifact.read_bytes():
        return [f"{identifier}: enabled Linux virtual displays require the "
                "amaterasu-virtual-display/linux-libvirtualdisplay build feature"]
    return []


def source_state(tree: Path) -> dict:
    """Identifies the exact source, including uncommitted and untracked files."""
    digest = source_diff_digest(tree)
    submodules = {}
    for line in git(tree, "config", "-f", ".gitmodules", "--get-regexp", "path$").decode().splitlines():
        path = line.split(maxsplit=1)[1]
        checkout = tree / path
        submodules[path] = git(checkout, "rev-parse", "HEAD").decode().strip()
        digest.update(path.encode() + b"\0" + source_diff_digest(checkout).digest())
    return {
        "commit": git(tree, "rev-parse", "HEAD").decode().strip(),
        "uncommitted_sha256": digest.hexdigest(),
        "submodules": submodules,
    }


def source_diff_digest(checkout: Path):
    digest = hashlib.sha256(git(checkout, "diff", "HEAD", "--binary", "--ignore-submodules=dirty"))
    untracked = git(checkout, "ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
    for name in sorted(name for name in untracked if name.endswith((b".rs", b".toml", b".c", b".h"))):
        digest.update(name + b"\0")
        digest.update((checkout / os.fsdecode(name)).read_bytes())
    return digest


def drift(provenance: dict, manifest: dict, running_host_sha256: str) -> list[str]:
    """Differences between the recorded install and what is actually selected."""
    problems = []
    recorded = provenance["components"]
    selected = {component["id"]: component["sha256"] for component in manifest["components"]}
    for component in sorted(set(recorded) | set(selected)):
        if recorded.get(component) != selected.get(component):
            problems.append(f"{component}: installed copy does not match the recorded build")
    if provenance["host_sha256"] != running_host_sha256:
        problems.append("host executable does not match the recorded build")
    return problems


def service_host() -> tuple[Path, str]:
    text = UNIT.read_text()
    path = re.search(r'^ExecStart="([^"]+)"', text, re.MULTILINE)
    digest = re.search(r"^# HostExecutableSha256=([0-9a-f]{64})", text, re.MULTILINE)
    if path is None or digest is None:
        raise SystemExit("the managed service unit does not name a pinned host")
    return Path(path.group(1)), digest.group(1)


def prepare(arguments) -> None:
    root, tree, work, host = (Path(value).resolve() for value in
                              (arguments.root, arguments.tree, arguments.work, arguments.host))
    work.mkdir(mode=0o700)
    selection = read_json(root / "selection.json")
    if selection["pending"] or selection["trial"]:
        raise SystemExit("an activation is already in progress")
    bootstrap = read_json(root / "bootstrap.json")
    if bootstrap["development"] or bootstrap["allow_unsigned"]:
        raise SystemExit("local updates target a signed production bootstrap")
    signing = core_signing(root, bootstrap, read_json(root / "package-activation-policy.json"))
    active = root / "sets" / selection["active"]
    manifest = read_json(active / "manifest.json")
    target = Path(arguments.target_dir).resolve() if arguments.target_dir else None
    allowlist = None
    if arguments.bound_input:
        from local_bound_input import prepare_bound_input
        allowlist = prepare_bound_input(bootstrap, manifest, read_json(root / "package-activation-policy.json"))
    if arguments.grant_changes:
        from local_grant_changes import apply_grant_changes
        approved = allowlist or read_json(root / "package-activation-policy.json")
        candidate, allowlist, delta, skipped = apply_grant_changes(
            bootstrap, manifest, approved, read_json(Path(arguments.grant_changes)))
        (work / "candidate-bootstrap.json").write_text(json.dumps(candidate, indent=2) + "\n")
        (work / "policy-delta.json").write_text(json.dumps(delta, indent=2) + "\n")
        print(f"grant changes: {len(delta)} policy paths; not in this set: {', '.join(skipped) or 'none'}")
    if allowlist is not None:
        (work / "package-activation-policy.json").write_bytes(compact(allowlist))

    problems = foreign_artifacts(tree, manifest["components"], target)
    problems.extend(missing_backend_features(
        bootstrap, manifest,
        {component["id"]: artifact_path(tree, component, target) for component in manifest["components"]}))
    if compiled_from(host) != tree:
        problems.append(f"host: compiled from {compiled_from(host)}, not {tree}")
    if problems:
        raise SystemExit("refusing a mixed build:\n  " + "\n  ".join(problems))

    package = core_package(work, manifest)
    package.mkdir(parents=True, mode=0o700)
    for component in manifest["components"]:
        artifact = artifact_path(tree, component, target)
        shutil.copy2(artifact, package / component["file"])
        component["sha256"] = sha256(artifact)
        component["size"] = artifact.stat().st_size
    manifest["sequence"] = selection["highest_sequence"] + 1
    if manifest["expires_unix"] <= time.time():
        raise SystemExit("the active manifest has expired")
    digest, catalog_digest = sign_core_bundle(package, manifest, signing)

    (work / "metadata.json").write_bytes(compact({
        "previous": selection["active"],
        "selection_revision": sha256(root / "selection.json"),
        "bootstrap_revision": sha256(root / "bootstrap.json"),
        "activation_policy_revision": sha256(root / "package-activation-policy.json"),
        "digest": digest,
        "catalog_digest": catalog_digest,
        "repository": CORE_REPOSITORY,
        "activation_policy_candidate_sha256": sha256(work / "package-activation-policy.json") if allowlist is not None else None,
    }))
    (work / PROVENANCE).write_text(json.dumps({
        "tree": str(tree),
        **source_state(tree),
        "built_at_unix": int(time.time()),
        "package_digest": digest,
        "components": {component["id"]: component["sha256"] for component in manifest["components"]},
        "host_sha256": sha256(host),
    }, indent=2))
    shutil.copy2(host, work / "amaterasu")
    shutil.copy2(host.parent / "amaterasu-release", work / "release-tool")
    print(f"prepared {len(manifest['components'])} components from {tree}: {digest}")


def live_sessions(root: Path) -> list:
    def post(path: str, body: dict, token: str | None = None) -> dict:
        connection = http.client.HTTPConnection("127.0.0.1", 48080, timeout=15)
        headers = {"Content-Type": "application/json", "Origin": "https://192.168.0.140:48081"}
        if token:
            headers["Authorization"] = "Bearer " + token
        connection.request("POST", path, json.dumps(body), headers)
        response = connection.getresponse()
        payload = json.loads(response.read())
        connection.close()
        if response.status != 200:
            raise OSError(f"{path} returned {response.status}")
        return payload

    password = (root / "qualification-admin-password.txt").read_text().strip()
    login = post("/v1/auth/login", {"username": "amaterasu-admin", "password": password})
    return post("/v1/live-streams/status", {}, login["bearer"])["sessions"]


def replace_host_pin(old_path: Path, old_sha: str, new_path: Path, new_sha: str) -> None:
    text = UNIT.read_text()
    if text.count(str(old_path)) != 1 or text.count(old_sha) != 1:
        raise SystemExit("the service unit changed unexpectedly")
    mode = UNIT.stat().st_mode & 0o777
    descriptor, temporary = tempfile.mkstemp(prefix=".amaterasu-unit-", dir=UNIT.parent)
    try:
        with os.fdopen(descriptor, "w") as output:
            output.write(text.replace(str(old_path), str(new_path)).replace(old_sha, new_sha))
            output.flush()
            os.fsync(output.fileno())
            os.fchmod(output.fileno(), mode)
        os.replace(temporary, UNIT)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def activate(arguments) -> None:
    root, work = Path(arguments.root).resolve(), Path(arguments.work).resolve()
    metadata = read_json(work / "metadata.json")
    for name, key in [("selection.json", "selection_revision"), ("bootstrap.json", "bootstrap_revision"),
                      ("package-activation-policy.json", "activation_policy_revision")]:
        if sha256(root / name) != metadata[key]:
            raise SystemExit(f"{name} changed since this update was prepared")
    if metadata.get("activation_policy_candidate_sha256") and sha256(work / "package-activation-policy.json") != metadata["activation_policy_candidate_sha256"]:
        raise SystemExit("the reviewed input policy changed; the host was left untouched")
    if live_sessions(root):
        raise SystemExit("a stream is active; the host was left untouched")

    old_host, _ = service_host()

    backup = work / "before-activation"
    backup.mkdir(mode=0o700)
    for name in ("selection.json", "bootstrap.json", "package-activation-policy.json"):
        shutil.copy2(root / name, backup / name)
    shutil.copy2(UNIT, backup / UNIT.name)
    shutil.copytree(root / "data/state", backup / "state", ignore_dangling_symlinks=True)

    subprocess.run([str(work / "release-tool"), "import-local-native", str(root), metadata["repository"],
                    str(work / "bundle"), metadata["digest"]], check=True)
    if live_sessions(root):
        raise SystemExit("a stream started during import; the update is cached but not active")
    for name, key in [("selection.json", "selection_revision"), ("bootstrap.json", "bootstrap_revision"),
                      ("package-activation-policy.json", "activation_policy_revision")]:
        if sha256(root / name) != metadata[key]:
            raise SystemExit(f"{name} changed during import; the host was left running")
    if metadata.get("activation_policy_candidate_sha256") and sha256(work / "package-activation-policy.json") != metadata["activation_policy_candidate_sha256"]:
        raise SystemExit("the reviewed input policy changed; the host was left running")
    subprocess.run(["systemctl", "--user", "stop", SERVICE], check=True)
    if metadata.get("activation_policy_candidate_sha256"):
        candidate = work / "package-activation-policy.json"
        path = root / "package-activation-policy.json"
        descriptor, temporary = tempfile.mkstemp(prefix=".activation-policy-", dir=root)
        try:
            with os.fdopen(descriptor, "wb") as output:
                original = path.stat()
                if os.fstat(output.fileno()).st_gid != original.st_gid:
                    os.fchown(output.fileno(), -1, original.st_gid)
                os.fchmod(output.fileno(), original.st_mode & 0o7777)
                output.write(candidate.read_bytes())
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, path)
            directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    subprocess.run([str(old_host), "activate-package-offline", str(root), metadata["repository"],
                    metadata["catalog_digest"], metadata["digest"], metadata["previous"],
                    metadata["selection_revision"]], check=True)
    if metadata["digest"] not in UNIT.read_text():
        raise SystemExit("activation did not advance the service package pin")
    # The trial is bound to the host that prepared it, so that host must finish
    # it. The executable can change only after the selection has committed.
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "start", SERVICE], check=True)
    wait_for_commit(root, metadata["digest"])
    install_matching_host(root, work)
    print(f"activated {metadata['digest']}")


def wait_for_commit(root: Path, digest: str) -> None:
    status = root / "data/state/package-activation-status.json"
    deadline = time.time() + COMMIT_TIMEOUT_S
    while time.time() < deadline:
        committed = status.is_file() and read_json(status) == {
            "digest": digest, "schema": 1, "status": "committed"}
        if committed and not (root / "package-activation-journal.json").exists():
            return
        time.sleep(1)
    raise SystemExit("the activation trial did not commit; the host was not replaced")


def wait_for_idle_host(root: Path) -> None:
    deadline = time.time() + COMMIT_TIMEOUT_S
    while time.time() < deadline:
        try:
            if live_sessions(root):
                raise SystemExit("a stream is active; the host was not replaced")
            return
        except (OSError, KeyError, ValueError):
            time.sleep(1)
    raise SystemExit("the host never answered; it was not replaced")


def install_matching_host(root: Path, work: Path) -> None:
    metadata = read_json(work / "metadata.json")
    provenance = read_json(work / PROVENANCE)
    wait_for_idle_host(root)
    directory = HOST_INSTALL_PARENT / f"install-full-{metadata['digest'][:12]}"
    directory.mkdir(mode=0o755, parents=True, exist_ok=True)
    new_host = directory / "amaterasu"
    if not new_host.exists():
        shutil.copy2(work / "amaterasu", new_host)
    new_host.chmod(0o755)
    if sha256(new_host) != provenance["host_sha256"]:
        raise SystemExit("the staged host does not match the recorded build")
    old_host, old_sha = service_host()
    if old_host == new_host:
        print("the matching host is already installed")
        return
    subprocess.run(["systemctl", "--user", "stop", SERVICE], check=True)
    replace_host_pin(old_host, old_sha, new_host, provenance["host_sha256"])
    shutil.copy2(work / PROVENANCE, root / PROVENANCE)
    subprocess.run(["systemctl", "--user", "daemon-reload"], check=True)
    subprocess.run(["systemctl", "--user", "start", SERVICE], check=True)
    print(f"host replaced with build {provenance['host_sha256'][:12]}")


def adopt_host(arguments) -> None:
    install_matching_host(Path(arguments.root).resolve(), Path(arguments.work).resolve())


def check(arguments) -> None:
    root = Path(arguments.root).resolve()
    record = root / PROVENANCE
    selection = read_json(root / "selection.json")
    manifest = read_json(root / "sets" / selection["active"] / "manifest.json")
    if not record.is_file():
        raise SystemExit(f"NOT IN SYNC: no {PROVENANCE}; the set was not installed by this tool")
    provenance = read_json(record)
    _, running_host = service_host()
    problems = drift(provenance, manifest, running_host)
    problems.extend(missing_backend_features(
        read_json(root / "bootstrap.json"), manifest,
        {component["id"]: root / "sets" / selection["active"] / component["file"]
         for component in manifest["components"]}))
    if provenance["package_digest"] != selection["active"]:
        problems.append("the selected package is not the one this tool installed")
    if problems:
        raise SystemExit("NOT IN SYNC:\n  " + "\n  ".join(problems))
    print(f"in sync: {len(manifest['components'])} components and the host from one build")
    print(f"source: {provenance['commit'][:12]} plus uncommitted {provenance['uncommitted_sha256'][:12]}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    prepare_parser = commands.add_parser("prepare")
    for name in ("root", "tree", "work", "host"):
        prepare_parser.add_argument(name)
    prepare_parser.add_argument("--target-dir")
    prepare_parser.add_argument("--bound-input", action="store_true", help="prepare exact external-capture input grants for review and activation")
    prepare_parser.add_argument("--grant-changes", help="JSON of reviewed grant additions and removals per principal (see local_grant_changes.py)")
    prepare_parser.set_defaults(run=prepare)
    activate_parser = commands.add_parser("activate")
    activate_parser.add_argument("root")
    activate_parser.add_argument("work")
    activate_parser.set_defaults(run=activate)
    adopt_parser = commands.add_parser("adopt-host")
    adopt_parser.add_argument("root")
    adopt_parser.add_argument("work")
    adopt_parser.set_defaults(run=adopt_host)
    check_parser = commands.add_parser("check")
    check_parser.add_argument("root")
    check_parser.set_defaults(run=check)
    arguments = parser.parse_args()
    arguments.run(arguments)


if __name__ == "__main__":
    sys.exit(main())
