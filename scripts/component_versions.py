#!/usr/bin/env python3
"""Print the released version of each component in a packed native set.

A packed manifest names each component's library file. This finds the crate
in the source tree that builds that library and reports the crate's version,
as the JSON object `amaterasu-release publish-repository --versions` reads.
Crates in this workspace share the workspace version; a module mounted from
its own repository carries its own.

    component_versions.py TREE MANIFEST_JSON [--extra-manifest CARGO_TOML ...]
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys


def libraries(tree, manifest_path=None):
    """Version by library name, for every crate that builds a loadable library."""
    command = ["cargo", "metadata", "--format-version", "1", "--no-deps", "--locked"]
    if manifest_path is not None:
        command += ["--manifest-path", str(manifest_path)]
    metadata = json.loads(subprocess.run(command, cwd=tree, check=True, capture_output=True).stdout)
    found = {}
    for package in metadata["packages"]:
        for target in package["targets"]:
            if "cdylib" in target["kind"]:
                found[target["name"].replace("-", "_")] = package["version"]
    return found


def library_name(file_name):
    """`libamaterasu_input.so` and `amaterasu_input.dll` both name amaterasu_input."""
    stem = file_name.split(".", 1)[0]
    return stem[3:] if stem.startswith("lib") else stem


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tree", type=Path, help="source tree the set was built from")
    parser.add_argument("manifest", type=Path, help="the packed set's manifest.json")
    parser.add_argument("--extra-manifest", action="append", default=[], type=Path, metavar="CARGO_TOML",
                        help="a crate built outside the workspace, such as modules/capture-vkms/capture/Cargo.toml")
    args = parser.parse_args()
    tree = args.tree.resolve(strict=True)
    known = libraries(tree)
    for extra in args.extra_manifest:
        known.update(libraries(tree, tree / extra))
    versions = {}
    unknown = []
    for component in json.loads(args.manifest.read_text(encoding="utf-8"))["components"]:
        version = known.get(library_name(component["file"]))
        if version is None:
            unknown.append(component["id"])
        else:
            versions[component["id"]] = version
    if unknown:
        sys.exit("no crate in the tree builds: " + ", ".join(unknown))
    json.dump(versions, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
