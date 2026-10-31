#!/usr/bin/env python3
"""Prepare a reviewable, non-mutating split of a legacy settings document.

Inputs are an exported registry document, its original descriptor, both candidate
descriptors, and explicit virtual-screen enabled choices for every source scope
and the host scope. Output is a new portable JSON file; no registry is contacted.
"""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import subprocess
import tempfile
import unicodedata
from pathlib import Path

DISPLAY = "nonary.display-settings"
VIRTUAL = "nonary.virtual-screen"
MOVED = {"retention", "retention_seconds", "keepalive_while_paused", "enforce_while_paused"}
SCOPES = {"host", "application", "paired_client", "session"}


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load(path: Path) -> tuple[bytes, object]:
    data = path.read_bytes()
    if len(data) > 1024 * 1024:
        raise ValueError("input exceeds 1 MiB")
    return data, json.loads(data, object_pairs_hook=unique_object)


def scope_key(scope: object) -> tuple[str, str]:
    if not isinstance(scope, dict) or scope.get("kind") not in SCOPES:
        raise ValueError("invalid scope")
    kind = scope["kind"]
    if kind == "host":
        if set(scope) != {"kind"}:
            raise ValueError("invalid host scope")
        return kind, ""
    value = scope.get("id")
    if set(scope) != {"kind", "id"} or not isinstance(value, str) or not value or len(value.encode()) > 256 or any(unicodedata.category(c) == "Cc" for c in value):
        raise ValueError("invalid scoped identity")
    return kind, value


def descriptor_options(descriptor: object, provider: str, revision: int) -> dict:
    if not isinstance(descriptor, dict) or descriptor.get("provider") != provider or descriptor.get("revision") != revision:
        raise ValueError("unexpected descriptor identity or revision")
    options = descriptor.get("options")
    if not isinstance(options, list) or not options or len(options) > 128:
        raise ValueError("invalid descriptor options")
    result = {}
    for option in options:
        if not isinstance(option, dict) or not isinstance(option.get("id"), str) or option["id"] in result:
            raise ValueError("invalid or duplicate option")
        scopes = option.get("allowed_scopes")
        if not isinstance(scopes, list) or not scopes or len(set(scopes)) != len(scopes) or not set(scopes) <= SCOPES:
            raise ValueError("invalid option scopes")
        validate_value(option.get("definition"), option.get("definition", {}).get("default"))
        result[option["id"]] = option
    return result


def validate_value(definition: object, value: object) -> None:
    if not isinstance(definition, dict):
        raise ValueError("invalid option definition")
    kind = definition.get("type")
    if kind == "boolean":
        valid = type(value) is bool
    elif kind == "enum":
        choices = definition.get("options")
        valid = isinstance(choices, list) and bool(choices) and all(isinstance(item, str) for item in choices) and len(set(choices)) == len(choices) and isinstance(value, str) and value in choices
    elif kind == "integer":
        low, high = definition.get("minimum"), definition.get("maximum")
        valid = type(low) is int and type(high) is int and type(value) is int and low <= value <= high
    elif kind == "string":
        low, high = definition.get("min_length"), definition.get("max_length")
        valid = type(low) is int and type(high) is int and isinstance(value, str) and low <= len(value) <= high
    else:
        valid = False
    if not valid:
        raise ValueError("value or schema is invalid or unsupported")


def validate_layer(options: dict, scope: dict, values: object) -> None:
    if not isinstance(values, dict):
        raise ValueError("layer values must be an object")
    kind, _ = scope_key(scope)
    for name, value in values.items():
        if name not in options or kind not in options[name]["allowed_scopes"]:
            raise ValueError(f"unsupported setting or scope: {name}")
        validate_value(options[name]["definition"], value)


def split(document: object, source: object, display: object, virtual: object, choices: object) -> dict:
    old = descriptor_options(source, DISPLAY, 1)
    new = descriptor_options(display, DISPLAY, 2)
    owner = descriptor_options(virtual, VIRTUAL, 1)
    if set(owner) != MOVED | {"enabled"} or set(old) != set(new) | MOVED or set(new) & MOVED or "enabled" not in new:
        raise ValueError("unsupported split descriptor fields")
    for name, destination in [(name, new[name]) for name in new] + [(name, owner[name]) for name in MOVED | {"enabled"}]:
        if old[name]["definition"] != destination["definition"] or set(old[name]["allowed_scopes"]) != set(destination["allowed_scopes"]):
            raise ValueError(f"incompatible destination schema: {name}")
    if not isinstance(document, dict) or set(document) != {"revision", "schema_revision", "layers"} or type(document["revision"]) is not int or not 0 <= document["revision"] < 2**64 or type(document["schema_revision"]) is not int or document["schema_revision"] != 1 or not isinstance(document["layers"], list) or len(document["layers"]) > 256:
        raise ValueError("invalid legacy document")
    if not isinstance(choices, list):
        raise ValueError("enabled choices must be a list")
    enabled = {}
    for item in choices:
        if not isinstance(item, dict) or set(item) != {"scope", "enabled"} or type(item["enabled"]) is not bool:
            raise ValueError("invalid enabled choice")
        key = scope_key(item["scope"])
        if key in enabled:
            raise ValueError("duplicate enabled choice")
        enabled[key] = item
    layers = {}
    for layer in document["layers"]:
        if not isinstance(layer, dict) or set(layer) != {"scope", "values"}:
            raise ValueError("invalid layer")
        key = scope_key(layer["scope"])
        if key in layers:
            raise ValueError("duplicate source scope")
        validate_layer(old, layer["scope"], layer["values"])
        layers[key] = layer
    required = set(layers) | {("host", "")}
    if set(enabled) != required:
        raise ValueError("explicit enabled choice required for each source scope and host")
    display_layers, virtual_layers = [], []
    for key in sorted(required):
        choice = enabled[key]
        layer = layers.get(key, {"scope": choice["scope"], "values": {}})
        values = layer["values"]
        display_values = {name: value for name, value in values.items() if name not in MOVED}
        virtual_values = {name: value for name, value in values.items() if name in MOVED}
        virtual_values["enabled"] = choice["enabled"]
        validate_layer(new, layer["scope"], display_values)
        validate_layer(owner, layer["scope"], virtual_values)
        if display_values:
            display_layers.append({"scope": layer["scope"], "values": display_values})
        virtual_layers.append({"scope": layer["scope"], "values": virtual_values})
    return {"display": {"schema_revision": 2, "layers": display_layers}, "virtual_screen": {"schema_revision": 1, "layers": virtual_layers}}


def validate_with_publisher(publisher: Path, descriptor: Path, layers: list[dict]) -> None:
    # Registry precedence: host, application, paired client, session. Enumerate
    # all selectable combinations, with a bounded refusal for oversized exports.
    buckets = {kind: [] for kind in SCOPES}
    for layer in layers:
        buckets[scope_key(layer["scope"])[0]].append(layer["values"])
    hosts = buckets["host"] or [{}]
    batches = [layer["values"] for layer in layers]
    for combination in itertools.product(hosts, *([{}] + buckets[kind] for kind in ("application", "paired_client", "session"))):
        effective = {}
        for values in combination:
            effective.update(values)
        batches.append(effective)
        if len(batches) > 4096:
            raise ValueError("too many effective scope combinations to validate")
    subprocess.run([str(publisher), "extension-validate-settings", str(descriptor)],
                   input=json.dumps(batches), text=True, capture_output=True, timeout=30, check=True)


def import_plan(provider: str, draft: dict) -> dict:
    return {"provider": provider, "schema_revision": draft["schema_revision"],
            "requires_empty_destination": True,
            "steps": [{"operation": "settings_update", "scope": layer["scope"],
                       "expected_revision": revision, "values": layer["values"]}
                      for revision, layer in enumerate(draft["layers"])]}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("publisher", type=Path, help="release binary used to validate all three descriptors")
    for name in ("source_document", "source_descriptor", "display_descriptor", "virtual_descriptor", "enabled_choices", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    source_bytes, document = load(args.source_document)
    descriptor_inputs = [load(path) for path in (args.source_descriptor, args.display_descriptor, args.virtual_descriptor)]
    _, choices = load(args.enabled_choices)
    result = split(document, *(item[1] for item in descriptor_inputs), choices)
    # Validate the same bytes that were parsed, even if input files change later.
    with tempfile.TemporaryDirectory(prefix="amaterasu-split-descriptors-") as temporary:
        descriptor_digests = []
        paths = []
        for index, (data, _) in enumerate(descriptor_inputs):
            path = Path(temporary) / f"descriptor-{index}.json"
            path.write_bytes(data)
            paths.append(path)
            checked = subprocess.run([str(args.publisher), "extension-descriptor-digest", str(path)],
                                     text=True, capture_output=True, timeout=30, check=True)
            digest = checked.stdout.strip()
            if len(digest) != 64 or any(character not in "0123456789abcdef" for character in digest):
                raise ValueError("publisher returned an invalid descriptor digest")
            descriptor_digests.append(digest)
        for path, layers in zip(paths, (document["layers"], result["display"]["layers"],
                                        result["virtual_screen"]["layers"]), strict=True):
            validate_with_publisher(args.publisher, path, layers)
    result["import_plans"] = [import_plan(DISPLAY, result["display"]), import_plan(VIRTUAL, result["virtual_screen"])]
    result.update({"format": "amaterasu-session-extension-split/1", "source_document_hex": source_bytes.hex(),
                   "source_sha256": hashlib.sha256(source_bytes).hexdigest(),
                   "descriptor_sha256": descriptor_digests,
                   "requires_old_owner_cleanup": True,
                   "requires_descriptor_approval_and_provider_validation": True,
                   "enabled_choices": choices})
    descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(result, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()
