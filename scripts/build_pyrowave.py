#!/usr/bin/env python3
"""Build the pinned standalone MIT PyroWave C API without installing system files."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import urllib.request

SOURCES = [
    ("Themaister/pyrowave", "186f0393b77f7755953b5ecde994bb1cec2e4155", "97c30baed03a13f505f645276c737a6e36cfa43fe272d5b1b52bc358dc4cf6c5", "source"),
    ("Themaister/Granite", "b6cffd5ce81f540f0855e6778428483e14763d9b", "52771877be6f00bff741fef3e8c0835721b2d8f045a53d2e5219187ec3863da1", "source/Granite"),
    ("zeux/volk", "47cddf7ed97b94118a08aacb548a411188e016cc", "ed771f9132ea077af0abc74d29f415a81f181f83c3ac5e0f67b2f2149976fe2d", "source/Granite/third_party/volk"),
    ("KhronosGroup/Vulkan-Headers", "6802bb4733b63ed5efd3adb308a6c885ef180ea1", "9044fbb356160e43201dc2094acb44c8a906722a63a35f48fc0f051579db9bb6", "source/Granite/third_party/khronos/vulkan-headers"),
]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=Path("native-deps/pyrowave"))
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    for repository, revision, digest, relative in SOURCES:
        archive_path = root / (repository.split("/")[-1] + ".tar.gz")
        if archive_path.exists():
            data = archive_path.read_bytes()
        else:
            with urllib.request.urlopen(f"https://codeload.github.com/{repository}/tar.gz/{revision}", timeout=120) as response:
                data = response.read()
        if hashlib.sha256(data).hexdigest() != digest:
            raise RuntimeError(f"Source archive digest mismatch: {repository}")
        archive_path.write_bytes(data)
        destination = root / relative
        destination.mkdir(parents=True, exist_ok=True)
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            for member in archive.getmembers():
                member.name = "/".join(member.name.split("/")[1:])
                if member.name:
                    archive.extract(member, destination, filter="data")
    patch_path = Path(__file__).with_name("pyrowave") / "import-ownership.patch"
    subprocess.run(["patch", "-p1", "--batch", "-i", str(patch_path.resolve())], cwd=root / "source", check=True)
    subprocess.run(["cmake", "-S", str(root / "source"), "-B", str(root / "build"), "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"], check=True)
    check_source = Path(__file__).with_name("check_pyrowave_abi.c")
    check_binary = root / "build/check-pyrowave-abi"
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-I", str(root / "source"),
                    "-I", str(root / "source/Granite/third_party/khronos/vulkan-headers/include"),
                    str(check_source), "-o", str(check_binary)], check=True)
    subprocess.run([str(check_binary)], check=True)
    # Extracted archives preserve upstream mtimes. A clean rebuild prevents an
    # older local object from surviving restoration of a modified source file.
    subprocess.run(["cmake", "--build", str(root / "build"), "--target", "pyrowave-shared", "--clean-first", "-j1"], check=True)
    print(json.dumps({"bitstream": SOURCES[0][1], "library_directory": str(root / "build")}, indent=2))

if __name__ == "__main__":
    main()
