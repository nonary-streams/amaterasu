#!/usr/bin/env python3
"""Refresh a checkout of the public SDK repository from committed core source.

The SDK repository (nonary-streams/amaterasu-sdk) is a snapshot of the shared
crates, C headers, examples and guides that module authors build against. Its
SDK_EXPORT.json records what was exported and from which core commit. This
tool reads that record, copies the same selection from a newer core commit,
and rewrites the root workspace manifest and the record to match.

Only committed bytes are exported: nothing is read from the working tree
except a submodule's own committed objects. The SDK checkout's history is not
touched; review and commit the result there.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tomllib

CORE = Path(__file__).resolve().parents[1]
DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")
NUMBER_WORDS = {
    14: "fourteen", 15: "fifteen", 16: "sixteen", 17: "seventeen", 18: "eighteen",
    19: "nineteen", 20: "twenty", 21: "twenty-one", 22: "twenty-two",
}


def git(*arguments, cwd=CORE):
    return subprocess.run(["git", *arguments], cwd=cwd, check=True, capture_output=True).stdout


def committed(revision, path):
    return git("cat-file", "blob", f"{revision}:{path}")


def committed_files(revision, directory):
    """Regular files below `directory` at `revision`; a submodule is not one."""
    listing = git("ls-tree", "-r", "-z", revision, "--", directory).decode()
    files = []
    for entry in filter(None, listing.split("\0")):
        header, path = entry.split("\t", 1)
        if header.split()[1] == "blob":
            files.append(path)
    return files


def local_dependencies(revision, member, workspace_dependencies):
    """Path dependencies of one member, as (kind, name, repository path)."""
    manifest = tomllib.loads(committed(revision, f"{member}/Cargo.toml").decode())
    tables = [manifest, *manifest.get("target", {}).values()]
    found = []
    for table in tables:
        for kind in DEPENDENCY_TABLES:
            for name, dependency in table.get(kind, {}).items():
                if not isinstance(dependency, dict):
                    continue
                if dependency.get("workspace") is True:
                    shared = workspace_dependencies.get(name)
                    path = shared.get("path") if isinstance(shared, dict) else None
                elif "path" in dependency:
                    path = normalize(Path(member) / dependency["path"])
                else:
                    path = None
                if path is not None:
                    found.append((kind, name, path))
    return found


def normalize(path):
    parts = []
    for part in Path(path).parts:
        if part == "..":
            if not parts:
                raise ValueError(f"{path} leaves the repository")
            parts.pop()
        elif part != ".":
            parts.append(part)
    return "/".join(parts)


def closure(revision, members, workspace_dependencies):
    """`members` with every crate they reach by path, and the edges between them."""
    reached = list(dict.fromkeys(members))
    edges = []
    index = 0
    while index < len(reached):
        member = reached[index]
        index += 1
        for kind, name, path in local_dependencies(revision, member, workspace_dependencies):
            # The record describes how the exported crates depend on each
            # other; an example's own dependencies are not part of that.
            if member.startswith("crates/"):
                edges.append({"from": member, "name": name, "to": path, "kind": kind})
            if path not in reached:
                reached.append(path)
    return reached, sorted(edges, key=edge_order)


def edge_order(edge):
    return edge["from"], edge["kind"], edge["name"]


def ordered(members):
    """Crates first, then everything else, each alphabetically."""
    crates = sorted(member for member in members if member.startswith("crates/"))
    return crates + sorted(member for member in members if not member.startswith("crates/"))


def workspace_manifest(revision, members):
    root = committed(revision, "Cargo.toml").decode()
    parsed = tomllib.loads(root)["workspace"]
    names = {}
    for member in members:
        names[member] = tomllib.loads(committed(revision, f"{member}/Cargo.toml").decode())["package"]["name"]
    section = re.search(r"(?ms)^\[workspace\.dependencies\]\n(.*?)(?=^\[)", root)
    if not section:
        raise ValueError("core root manifest has no [workspace.dependencies] table")
    external = [
        line for line in section.group(1).splitlines()
        if line.strip() and "path =" not in line and "path=" not in line
    ]
    package = parsed["package"]
    lines = ["[workspace]", 'resolver = "2"', "members = ["]
    lines += [f"    {json.dumps(member)}," for member in members]
    lines += ["]", "", "[workspace.package]"]
    lines += [f"{key} = {json.dumps(package[key])}" for key in ("edition", "rust-version", "version")]
    lines += ['license = "MIT"', "", "[workspace.dependencies]"]
    lines += [
        f"{names[member]} = {{ path = {json.dumps(member)} }}"
        for member in members if member.startswith("crates/")
    ]
    lines += external
    lines.append("")
    for group, entries in parsed.get("lints", {}).items():
        lines.append(f"[workspace.lints.{group}]")
        lines += [f"{name} = {json.dumps(level)}" for name, level in entries.items()]
    return "\n".join(lines) + "\n"


def fixture_bytes(revision, fixture):
    """A contract fixture's committed bytes, from core or a pinned submodule."""
    if fixture["source_repository"] == "core":
        return revision, committed(revision, fixture["path"])
    mount = "/".join(fixture["path"].split("/")[:2])
    entry = git("ls-tree", revision, "--", mount).decode().split()
    if len(entry) < 3 or entry[1] != "commit":
        raise ValueError(f"{mount} is not a submodule at {revision}")
    inner = fixture["path"][len(mount) + 1:]
    return entry[2], git("cat-file", "blob", f"{entry[2]}:{inner}", cwd=CORE / mount)


def plan(record, revision, new_seeds):
    """Every file the export holds, as {path: bytes}, with the updated record."""
    workspace_dependencies = tomllib.loads(committed(revision, "Cargo.toml").decode())["workspace"]["dependencies"]
    seeds = list(dict.fromkeys(record["seed_crates"] + new_seeds))
    wanted = record["workspace_members"] + [f"crates/{seed}" for seed in new_seeds]
    members, edges = closure(revision, wanted, workspace_dependencies)
    members = ordered(members)
    excluded = record.get("excluded_source_paths", {})
    files = {}
    for member in members:
        for path in committed_files(revision, member):
            if not any(path.startswith(prefix) for prefix in excluded):
                files[path] = committed(revision, path)
    for path in committed_files(revision, "sdk"):
        if not any(path.startswith(prefix) for prefix in excluded):
            files.setdefault(path, committed(revision, path))
    present = set(git("ls-tree", "-r", "--name-only", revision, "--", "docs").decode().split("\n"))
    for entry in record["source_files"]:
        path = entry["path"]
        if path.startswith("docs/") and path in present:
            files[path] = committed(revision, path)
    fixtures = []
    fixture_files = {}
    for fixture in record.get("contract_test_fixtures", []):
        source_commit, content = fixture_bytes(revision, fixture)
        fixture_files[fixture["path"]] = content
        fixtures.append({**fixture, "source_commit": source_commit, "sha256": hashlib.sha256(content).hexdigest()})
    updated = dict(record)
    updated["core_source_commit"] = git("rev-parse", f"{revision}^{{commit}}").decode().strip()
    updated["seed_crates"] = seeds
    updated["workspace_members"] = members
    updated["local_dependency_edges"] = edges
    updated["source_files"] = [
        {"path": path, "sha256": hashlib.sha256(files[path]).hexdigest()} for path in sorted(files)
    ]
    updated["contract_test_fixtures"] = fixtures
    return members, files, fixture_files, updated


def describe(readme, updated):
    """Keep the generated README's opening facts in step with the record."""
    crates = sum(1 for member in updated["workspace_members"] if member.startswith("crates/"))
    readme = re.sub(r"(exports committed Amaterasu core `)[0-9a-f]{40}(`)",
                    lambda match: match.group(1) + updated["core_source_commit"] + match.group(2), readme, count=1)
    word = NUMBER_WORDS.get(crates, str(crates))
    return re.sub(r"The workspace contains [a-z0-9-]+ library crates",
                  f"The workspace contains {word} library crates", readme, count=1)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("sdk", type=Path, help="checkout of nonary-streams/amaterasu-sdk")
    parser.add_argument("--core", default="HEAD", help="core commit to export (default: HEAD)")
    parser.add_argument("--seed", action="append", default=[], metavar="CRATE",
                        help="also export crates/CRATE and everything it depends on")
    parser.add_argument("--check", action="store_true",
                        help="report what would change without writing")
    args = parser.parse_args()
    sdk = args.sdk.resolve(strict=True)
    record_path = sdk / "SDK_EXPORT.json"
    record = json.loads(record_path.read_text(encoding="utf-8"))
    if record.get("schema_version") != 1:
        sys.exit("SDK_EXPORT.json has an unsupported schema version")
    for seed in args.seed:
        if not re.fullmatch(r"[a-z0-9-]+", seed):
            sys.exit(f"not a crate name: {seed}")
    members, files, fixture_files, updated = plan(record, args.core, args.seed)
    updated["sdk_parent_commit"] = git("rev-parse", "HEAD", cwd=sdk).decode().strip()

    outputs = dict(files)
    outputs.update(fixture_files)
    outputs["Cargo.toml"] = workspace_manifest(args.core, members).encode()
    outputs["README.md"] = describe((sdk / "README.md").read_text(encoding="utf-8"), updated).encode()
    previous = {entry["path"] for entry in record["source_files"]}
    stale = sorted(path for path in previous if path not in files and (sdk / path).is_file())
    changed = sorted(
        path for path, content in outputs.items()
        if not (sdk / path).is_file() or (sdk / path).read_bytes() != content
    )
    for path in changed:
        print(("add     " if not (sdk / path).exists() else "update  ") + path)
    for path in stale:
        print("remove  " + path)
    serialized = json.dumps(updated, indent=2) + "\n"
    # Earlier records listed the same edges in traversal order.
    comparable = {**record, "sdk_parent_commit": None,
                  "local_dependency_edges": sorted(record["local_dependency_edges"], key=edge_order)}
    record_changed = comparable != {**updated, "sdk_parent_commit": None}
    if args.check:
        if changed or stale or record_changed:
            sys.exit(f"{len(changed)} file(s) differ, {len(stale)} stale: the SDK checkout is not this export")
        print("the SDK checkout matches this export")
        return
    for path in changed:
        destination = sdk / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(outputs[path])
    for path in stale:
        (sdk / path).unlink()
    if changed or stale or record_changed:
        record_path.write_text(serialized, encoding="utf-8")
    print(f"exported {len(files)} source files from core {updated['core_source_commit'][:12]} "
          f"into {len(members)} workspace members")


if __name__ == "__main__":
    main()
