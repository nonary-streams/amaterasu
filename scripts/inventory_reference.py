#!/usr/bin/env python3
"""Extract pinned reference route declarations without copying implementations."""
import json
from pathlib import Path
import re
import subprocess

WORKSPACE = Path(__file__).resolve().parents[1]
REFERENCE = "aa292c4cb097f22beab19d6ebf0307367f9bf449"
SOURCES = ("src/confighttp.cpp", "src/nvhttp.cpp")


def reference_text(path: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(WORKSPACE.parent / "vibeshine"), "show", f"{REFERENCE}:{path}"],
        text=True,
    )


def main() -> None:
    declarations = []
    registered = re.compile(r'register_(?:blocking_)?api_route\("([^"]+)", "([A-Z]+)", (\w+)\)')
    direct = re.compile(r'(\w+)\.resource\["([^"]+)"\]\["([A-Z]+)"\]')
    for path in SOURCES:
        conditions = []
        for number, line in enumerate(reference_text(path).splitlines(), 1):
            directive = line.strip()
            if directive.startswith(("#if ", "#ifdef ", "#ifndef ")):
                conditions.append(directive)
            elif directive.startswith(("#else", "#elif")) and conditions:
                conditions[-1] += f"; {directive}"
            elif directive.startswith("#endif") and conditions:
                conditions.pop()
            match = registered.search(line)
            if match:
                pattern, method, handler = match.groups()
                surface = "management"
            else:
                match = direct.search(line)
                if not match:
                    continue
                surface, pattern, method = match.groups()
                handler = None
            declarations.append({
                "source": path, "line": number, "surface": surface,
                "method": method, "pattern": pattern, "handler": handler,
                "preprocessor_context": conditions.copy(),
            })
    artifact = {
        "reference_commit": REFERENCE,
        "meaning": "Route declarations only; does not prove endpoint semantics or compatibility.",
        "routes": declarations,
    }
    destination = WORKSPACE / "docs" / "reference-routes.json"
    destination.write_text(json.dumps(artifact, indent=2) + "\n")
    print(f"Recorded {len(declarations)} pinned route declarations in {destination}")


if __name__ == "__main__":
    main()
