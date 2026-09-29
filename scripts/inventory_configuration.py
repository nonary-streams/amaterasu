#!/usr/bin/env python3
"""Build a deterministic configuration inventory from pinned Vibeshine sources."""

from __future__ import annotations

import ast
import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


WORKSPACE = Path(__file__).resolve().parents[1]
REFERENCE_REPO = Path(
    os.environ.get("AMATERASU_VIBESHINE_REFERENCE", str(WORKSPACE.parent / "vibeshine"))
)
if not REFERENCE_REPO.is_absolute():
    REFERENCE_REPO = WORKSPACE / REFERENCE_REPO
REFERENCE_COMMIT = "457237db7e57f9c4be21c35bb07e28d1c100eb7d"
OUTPUT = WORKSPACE / "docs" / "configuration-parity.json"
COVERAGE_OUTPUT = WORKSPACE / "docs" / "configuration-coverage.json"


def reference_text(path: str) -> str:
    return subprocess.check_output(
        ["git", "-C", str(REFERENCE_REPO), "show", f"{REFERENCE_COMMIT}:{path}"],
        text=True,
        encoding="utf-8",
    )


def add_source(
    inventory: dict[tuple[str, str], dict[str, Any]],
    key: str,
    scope: str,
    path: str,
    line: int,
    role: str,
    detail: str | None = None,
) -> None:
    identity = (scope, key)
    item = inventory.setdefault(identity, {"key": key, "scope": scope, "sources": []})
    source: dict[str, Any] = {"path": path, "line": line, "role": role}
    if detail:
        source["detail"] = detail
    if source not in item["sources"]:
        item["sources"].append(source)


def lines_with_pattern(text: str, pattern: str) -> list[tuple[int, re.Match[str]]]:
    expression = re.compile(pattern)
    return [(number, match) for number, line in enumerate(text.splitlines(), 1) if (match := expression.search(line))]


def parse_literal(value: str) -> tuple[Any, bool]:
    raw = value.strip().rstrip(",").strip()
    if raw in {"true", "false"}:
        return raw == "true", True
    if raw == "null":
        return None, True
    if raw in {"[]", "{}"}:
        return ([] if raw == "[]" else {}), True
    try:
        return ast.literal_eval(raw), True
    except (SyntaxError, ValueError):
        try:
            return json.loads(raw), True
        except (json.JSONDecodeError, TypeError):
            return None, False


def object_body(text: str, assignment: str) -> tuple[str, int]:
    match = re.search(rf"\b{re.escape(assignment)}\b(?:\s*:[^=]+)?\s*=\s*\{{", text)
    if not match:
        return "", 0
    start = match.end() - 1
    depth = 0
    quote: str | None = None
    escaped = False
    for index in range(start, len(text)):
        char = text[index]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in "'\"`":
            quote = char
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[start + 1 : index], text.count("\n", 0, start) + 1
    return "", 0


def object_defaults(text: str, assignment: str) -> tuple[dict[str, Any], dict[str, str], dict[str, int]]:
    body, start_line = object_body(text, assignment)
    values: dict[str, Any] = {}
    expressions: dict[str, str] = {}
    source_lines: dict[str, int] = {}
    if not body:
        return values, expressions, source_lines
    for offset, line in enumerate(body.splitlines()):
        match = re.match(r"^  ([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.*?)(?:,\s*)?$", line)
        if not match:
            continue
        key, expression = match.groups()
        source_lines[key] = start_line + offset + 1
        value, known = parse_literal(expression)
        if known:
            values[key] = value
        else:
            expressions[key] = expression.strip().rstrip(",")
    return values, expressions, source_lines


def balanced_call(text: str, opening: int) -> str:
    depth = 0
    quote: str | None = None
    escaped = False
    for index in range(opening, len(text)):
        char = text[index]
        if quote:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in "'\"`":
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return text[opening + 1 : index]
    return text[opening + 1 :]


def type_from_default(value: Any) -> str | None:
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        return "object"
    return None


def extract_schema(
    inventory: dict[tuple[str, str], dict[str, Any]],
    schema: str,
    legacy_schema: str,
    extended: str,
) -> tuple[set[str], set[str], dict[str, str], dict[str, Any], dict[str, list[str]]]:
    config_keys: set[str] = set()
    client_keys: set[str] = set()
    web_types: dict[str, str] = {}
    defaults: dict[str, Any] = {}
    platforms: dict[str, list[str]] = defaultdict(list)

    kind_to_type = {
        "boolean": "boolean",
        "number": "number",
        "text": "string",
        "select": "string",
        "duration": "number",
        "integration-path": "string",
        "textarea": "string",
        "mode-remapping": "object",
        "display-recovery": "object",
        "command-preparations": "array",
    }

    for source_path, source_text in ((schema, reference_text(schema)), (legacy_schema, reference_text(legacy_schema))):
        lines = source_text.splitlines()
        in_client_set = False
        for line_number, line in enumerate(lines, 1):
            if "export const clientOverrideableKeys = new Set([" in line:
                in_client_set = True
                continue
            if in_client_set and re.match(r"\s*\]\);", line):
                in_client_set = False
                continue
            for match in re.finditer(
                r"\b(boolean|number|text|select|duration|extendedField|integrationPath)\(\s*'([a-z][a-z0-9_]*)'",
                line,
            ):
                factory, key = match.groups()
                config_keys.add(key)
                add_source(inventory, key, "global", source_path, line_number, "web_field")
                if factory != "extendedField":
                    web_types.setdefault(key, kind_to_type.get(factory, "string"))
                else:
                    add_source(inventory, key, "global", extended, 83, "extended_field_factory")
                tail = source_text[sum(len(part) + 1 for part in lines[: line_number - 1]) + match.start() :]
                call = balanced_call(tail, tail.index("(") )
                platform_match = re.search(r"\bplatform\s*:\s*(\[[^\]]*\]|'[^']+')", call)
                if platform_match:
                    values = re.findall(r"'([a-z]+)'", platform_match.group(1))
                    platforms[key].extend(value for value in values if value not in platforms[key])

            direct = re.search(r"\bkey\s*:\s*'([a-z][a-z0-9_]*)'", line)
            if direct:
                key = direct.group(1)
                config_keys.add(key)
                add_source(inventory, key, "global", source_path, line_number, "web_field")
                for kind, value in kind_to_type.items():
                    if re.search(rf"\bkind\s*:\s*'{re.escape(kind)}'", line):
                        web_types.setdefault(key, value)
                object_start = next(
                    (index for index in range(line_number - 1, -1, -1) if "{" in lines[index]),
                    line_number - 1,
                )
                brace_depth = 0
                object_lines: list[str] = []
                for object_line in lines[object_start:]:
                    object_lines.append(object_line)
                    brace_depth += object_line.count("{") - object_line.count("}")
                    if brace_depth == 0:
                        break
                object_platform = re.search(r"\bplatform\s*:\s*(\[[^\]]*\]|'[^']+')", "\n".join(object_lines))
                if object_platform:
                    values = re.findall(r"'([a-z]+)'", object_platform.group(1))
                    platforms[key].extend(value for value in values if value not in platforms[key])

            if in_client_set:
                for value in re.findall(r"'([a-z][a-z0-9_]*)'", line):
                    client_keys.add(value)
                    add_source(inventory, value, "global", source_path, line_number, "client_override_allowlist")

    ext_text = reference_text(extended)
    ext_defaults, ext_expressions, ext_lines = object_defaults(ext_text, "extendedDefaults")
    for key, value in ext_defaults.items():
        defaults.setdefault(key, value)
        config_keys.add(key)
        add_source(inventory, key, "global", extended, ext_lines[key], "extended_default")
        if key not in web_types:
            web_types[key] = type_from_default(value) or "string"
    for key, expression in ext_expressions.items():
        config_keys.add(key)
        add_source(inventory, key, "global", extended, ext_lines[key], "extended_default", expression)
        web_types.setdefault(key, "object")

    # The remaining defaults are the host's settings defaults, including simple everyday fields.
    settings_text = reference_text(schema)
    setting_defaults, setting_expressions, settings_lines = object_defaults(settings_text, "settingsDefaults")
    for key, value in setting_defaults.items():
        defaults[key] = value
        config_keys.add(key)
        add_source(inventory, key, "global", schema, settings_lines[key], "settings_default")
        web_types.setdefault(key, type_from_default(value) or "string")
    for key, expression in setting_expressions.items():
        config_keys.add(key)
        add_source(inventory, key, "global", schema, settings_lines[key], "settings_default", expression)
        web_types.setdefault(key, "object")

    # Provider policy editors expose additional settings through destinations and integration views.
    for source_path in (
        "src_assets/common/assets/web/configs/settingsSchema.ts",
        "src_assets/common/assets/web/views/IntegrationsView.vue",
    ):
        source_text = reference_text(source_path)
        for match in re.finditer(r"['\"]((?:steam|lutris|playnite)_[a-z][a-z0-9_]*)['\"]", source_text):
            key = match.group(1)
            config_keys.add(key)
            line_number = source_text.count("\n", 0, match.start()) + 1
            add_source(inventory, key, "global", source_path, line_number, "web_integration_setting")
            if key not in web_types:
                web_types[key] = "string"

    # Extended settings create family-specific field lists from prefixes at runtime.
    for key in ext_defaults:
        if key.startswith("playnite_"):
            platforms[key] = ["windows"]
        elif key.startswith(("qsv_", "amd_")):
            platforms[key] = ["windows"]
        elif key.startswith("vt_"):
            platforms[key] = ["macos"]
        elif key.startswith(("vaapi_", "vk_")):
            platforms[key] = ["linux"]
        elif key in {"nvenc_realtime_hags", "nvenc_opengl_vulkan_on_dxgi"}:
            platforms[key] = ["windows"]
        elif key.startswith("rtx_hdr"):
            platforms[key] = ["windows"]
    return config_keys, client_keys, web_types, defaults, platforms


def docs_headings(text: str) -> dict[str, tuple[str, int]]:
    domain = "configuration"
    headings: dict[str, tuple[str, int]] = {}
    for line_number, line in enumerate(text.splitlines(), 1):
        if line.startswith("## "):
            domain = line[3:].strip()
        elif line.startswith("### "):
            key = line[4:].strip()
            if re.fullmatch(r"[a-z][a-z0-9_]*", key):
                headings[key] = (domain, line_number)
    return headings


def source_platform(source_paths: set[str], key: str, schema_platforms: dict[str, list[str]]) -> list[str] | None:
    if schema_platforms.get(key):
        return sorted(set(schema_platforms[key]))
    if key.startswith("playnite_") or key in {"rtss_install_path", "rtss_frame_limit_type", "rtss_allow_virtual_display_override", "rtss_disable_vsync_ullm"}:
        return ["windows"]
    if key in {"dd_virtual_display_permanent_count", "dd_vdd_static_monitor_count", "dd_wa_hdr_toggle", "dd_wa_hdr_toggle_delay"}:
        return ["windows"]
    if key.startswith("lutris_") or key in {"vulkan_hdr_layer", "wayland_hdr_compatibility"}:
        return ["linux"]
    if key.startswith(("qsv_", "amd_")):
        return ["windows"]
    if key.startswith("vt_"):
        return ["macos"]
    if key.startswith(("vaapi_", "vk_")):
        return ["linux"]
    if key.startswith("nvenc_") and key in {"nvenc_realtime_hags", "nvenc_opengl_vulkan_on_dxgi"}:
        return ["windows"]
    if "config_playnite" in source_paths or "src/config_playnite_parse.cpp" in source_paths:
        return ["windows"]
    if "config_lutris" in source_paths:
        return ["linux"]
    if "src/config_steam.cpp" in source_paths:
        return ["windows", "linux", "macos"]
    if source_paths.intersection(
        {
            "src_assets/common/assets/web/configs/settingsSchema.ts",
            "src_assets/common/assets/web-legacy/configs/configFieldSchema.ts",
            "src_assets/common/assets/web/configs/extendedSettings.ts",
        }
    ):
        # An omitted platform property passes matchesPlatform for every host UI.
        return ["windows", "linux", "macos"]
    return None


def infer_domain(key: str, docs_domain: str | None, scope: str) -> str:
    if docs_domain:
        return docs_domain
    if scope == "per_application":
        return "Application catalog and launch policy"
    prefixes = {
        "playnite_": "Playnite Integration",
        "steam_": "Steam Integration",
        "lutris_": "Lutris Integration",
        "rtss_": "Frame Limiter",
        "mangohud_": "Frame Limiter",
        "frame_limiter_": "Frame Limiter",
        "nvenc_": "NVIDIA NVENC Encoder",
        "qsv_": "Intel QuickSync Encoder",
        "amd_": "AMD AMF Encoder",
        "vt_": "VideoToolbox Encoder",
        "vaapi_": "VA-API Encoder",
        "vk_": "Vulkan Encoder",
        "sw_": "Software Encoder",
        "dd_": "Audio/Video",
        "rtx_hdr": "Advanced runtime and recovery options",
        "realtime_stats_": "Advanced runtime and recovery options",
        "session_history_": "Advanced runtime and recovery options",
    }
    for prefix, domain in prefixes.items():
        if key.startswith(prefix):
            return domain
    if key in {"controller", "gamepad", "keyboard", "mouse", "keybindings", "back_button_timeout"} or key.startswith(("key_", "ds4_", "ds5_", "proton_", "touchpad_", "motion_")):
        return "Input"
    if key in {"audio_sink", "virtual_sink", "stream_audio", "audio_sink_capture_only", "install_steam_audio_drivers"}:
        return "Audio/Video"
    if key in {"fec_percentage", "qp", "min_threads", "capture", "encoder", "max_bitrate", "minimum_fps_target", "hevc_mode", "av1_mode", "video_max_batch_size_kb"}:
        return "Advanced"
    if key in {"bind_address", "address_family", "port", "upnp", "external_ip", "origin_web_ui_allowed", "csrf_allowed_origins", "lan_encryption_mode", "wan_encryption_mode", "ping_timeout"}:
        return "Network"
    if key in {"flags"}:
        return "Advanced (legacy flags)"
    return "General"


def parser_entries(
    inventory: dict[tuple[str, str], dict[str, Any]],
    parser: str,
    parser_keys: set[str],
    parser_types: dict[str, str],
) -> None:
    text = reference_text(parser)
    types = {
        "bool_f": "boolean",
        "int_f": "integer",
        "int_between_f": "integer",
        "double_f": "number",
        "double_between_f": "number",
        "frame_limit_millihz_f": "number",
        "string_f": "string",
        "path_f": "path",
        "string_restricted_f": "enum",
        "generic_f": "enum",
        "list_string_f": "array[string]",
        "list_int_f": "array[integer]",
        "map_int_int_f": "map[integer,integer]",
        "list_prep_cmd_f": "array[preparation_command]",
    }
    # Match across lines because a few bounded parser calls format the key on its own line.
    for match in re.finditer(r"\b([A-Za-z_][A-Za-z0-9_]*)\(\s*vars\s*,\s*\"([^\"]+)\"", text):
        helper, key = match.groups()
        if helper in {"parse_option", "reference_text"}:
            continue
        line_number = text.count("\n", 0, match.start()) + 1
        parser_keys.add(key)
        add_source(inventory, key, "global", parser, line_number, "config_parser", helper)
        if helper in types:
            parser_types.setdefault(key, types[helper])
    for match in re.finditer(r"\bvars\.(?:find|contains|erase)\(\s*\"([^\"]+)\"", text):
        key = match.group(1)
        if key in {"apps", "env"}:
            continue
        line_number = text.count("\n", 0, match.start()) + 1
        parser_keys.add(key)
        add_source(inventory, key, "global", parser, line_number, "config_parser_lookup")

    ui_only_start = text.find("for (const auto *ui_only_key : {")
    ui_only_end = text.find("})", ui_only_start)
    if ui_only_start >= 0 and ui_only_end >= 0:
        ui_only = text[ui_only_start:ui_only_end]
        for match in re.finditer(r'"([a-z][a-z0-9_]+)"', ui_only):
            key = match.group(1)
            line_number = text.count("\n", 0, ui_only_start + match.start()) + 1
            parser_keys.add(key)
            add_source(inventory, key, "global", parser, line_number, "web_only_config_key")


def specialized_parser_entries(inventory: dict[tuple[str, str], dict[str, Any]]) -> dict[str, str]:
    files = {
        "src/config_steam.cpp": "Steam Integration",
        "src/config_lutris.cpp": "Lutris Integration",
        "src/config_playnite_parse.cpp": "Playnite Integration",
    }
    result: dict[str, str] = {}
    patterns = (
        r'\b(?:consume|consume_bool|consume_int|take|erase_take)\(\s*vars\s*,\s*"([a-z][a-z0-9_]+)"',
        r'\b(?:parse_exclusions|parse_id_name_array)\(\s*vars\s*,\s*"([a-z][a-z0-9_]+)"',
    )
    for path, domain in files.items():
        text = reference_text(path)
        for pattern in patterns:
            for line_number, match in lines_with_pattern(text, pattern):
                key = match.group(1)
                add_source(inventory, key, "global", path, line_number, "specialized_config_parser", domain)
                call_line = text.splitlines()[line_number - 1]
                if "consume_bool(" in call_line:
                    result[key] = "boolean"
                elif "consume_int(" in call_line:
                    result[key] = "integer"
                elif "parse_id_name_array(" in call_line or key.endswith("exclude_games"):
                    result[key] = "array[identifier]"
                else:
                    result.setdefault(key, "string")
    return result


def specialized_host_defaults() -> dict[str, tuple[str, Any, str, int]]:
    declarations = (
        ("src/config_steam.h", "steam_t", "steam_"),
        ("src/config_lutris.h", "lutris_t", "lutris_"),
        ("src/config_playnite.h", "playnite_t", "playnite_"),
    )
    result: dict[str, tuple[str, Any, str, int]] = {}
    type_names = {
        "bool": "boolean",
        "int": "integer",
        "std::string": "string",
        "std::vector<std::string>": "array[string]",
        "std::vector<id_name_t>": "array[identifier]",
    }
    for path, struct_name, prefix in declarations:
        text = reference_text(path)
        match = re.search(rf"struct\s+{struct_name}\s*\{{(.*?)\n\s*\}};", text, re.S)
        if not match:
            raise ValueError(f"specialized settings struct {struct_name} was not found in {path}")
        body = match.group(1)
        base_line = text.count("\n", 0, match.start(1)) + 1
        for offset, line in enumerate(body.splitlines()):
            declaration = re.match(
                r"\s*(bool|int|std::string|std::vector<std::string>|std::vector<id_name_t>)\s+([a-z][a-z0-9_]*)\s*(?:=\s*([^;]+))?;",
                line,
            )
            if not declaration:
                continue
            type_name, field_name, expression = declaration.groups()
            if field_name.endswith("_meta"):
                continue
            key = prefix + field_name
            value: Any = [] if type_name.startswith("std::vector") else None
            if expression:
                value, known = parse_literal(expression)
                if not known:
                    value = None
            result[key] = (type_names[type_name], value, path, base_line + offset)

    # Authentication defaults are initialized in the global config aggregate,
    # rather than in a dedicated integration struct. Keep the distinct web
    # default so a disagreement remains visible in the generated inventory.
    config_lines = reference_text("src/config.cpp").splitlines()
    authentication_defaults = (
        (
            "session_token_ttl_seconds",
            r"std::chrono::hours\s*\{\s*2\s*\}.*//\s*session_token_ttl default 2h",
            2 * 60 * 60,
        ),
        (
            "remember_me_refresh_token_ttl_seconds",
            r"std::chrono::hours\s*\{\s*24\s*\*\s*7\s*\}.*//\s*remember_me_refresh_token_ttl default 7d",
            7 * 24 * 60 * 60,
        ),
    )
    for key, pattern, seconds in authentication_defaults:
        matching_lines = [
            number
            for number, line in enumerate(config_lines, 1)
            if re.search(pattern, line)
        ]
        if len(matching_lines) != 1:
            raise ValueError(f"authentication host default for {key} was not found exactly once")
        result[key] = ("integer", seconds, "src/config.cpp", matching_lines[0])
    return result


def backend_override_keys(text: str) -> tuple[set[str], dict[str, int]]:
    marker = "bool is_allowed_override_key"
    start = text.find(marker)
    if start < 0:
        raise ValueError("backend override allow-list was not found")
    end = text.find("return kAllowed.contains(key)", start)
    if end < 0:
        raise ValueError("backend override allow-list is not closed as expected")
    block = text[start:end]
    result: dict[str, int] = {}
    base_line = text.count("\n", 0, start) + 1
    for match in re.finditer(r'"([a-z][a-z0-9_]+)"', block):
        key = match.group(1)
        result[key] = base_line + block.count("\n", 0, match.start())
    return set(result), result


def app_entry_keys(text: str) -> list[tuple[str, str, int]]:
    result: list[tuple[str, str, int]] = []
    lines = text.splitlines()
    in_parse = False
    for line_number, line in enumerate(lines, 1):
        if "std::optional<proc::proc_t> parse(" in line:
            in_parse = True
        if not in_parse:
            continue
        if "app_node.get_optional<" in line or "app_node.get_child_optional(" in line or "app_node.get<" in line:
            match = re.search(r'\("([a-z][a-z0-9_-]+)"', line)
            if match:
                key = match.group(1)
                type_match = re.search(r"get_(?:child_)?optional<([^>]+)>|get<([^>]+)>", line)
                value_type = next((part.strip() for part in type_match.groups() if part), "structured") if type_match else "structured"
                type_map = {"bool": "boolean", "int": "integer", "std::string": "string"}
                result.append((key, type_map.get(value_type, "object-or-array"), line_number))
        if "japp.contains(\"config-overrides\")" in line:
            result.append(("config-overrides", "object[allowed_global_config_keys]", line_number))
    return result


def app_aliases(text: str) -> list[dict[str, Any]]:
    aliases = [
        ("dlss-framegen-capture-fix", "gen1-framegen-fix", "legacy per-app frame-generation field"),
    ]
    result = []
    for alias, canonical, reason in aliases:
        match = re.search(rf'get_optional<[^>]+>\("{re.escape(alias)}"', text)
        if not match:
            raise ValueError(f"pinned application parser no longer declares alias {alias}")
        result.append(
            {
                "alias": alias,
                "canonical": canonical,
                "scope": "per_application",
                "reason": reason,
                "source": {"path": "src/process.cpp", "line": text.count("\n", 0, match.start()) + 1},
            }
        )
    return result


def validate_implementation_coverage(ids: set[str], initialize: bool) -> None:
    if not COVERAGE_OUTPUT.exists():
        if not initialize:
            raise ValueError(f"{COVERAGE_OUTPUT} is missing; regenerate the inventory to initialize it")
        initial = {
            "reference_inventory": "configuration-parity.json",
            "reference_commit": REFERENCE_COMMIT,
            "status_values": ["pending", "verified"],
            "statuses": {identifier: "pending" for identifier in sorted(ids)},
        }
        COVERAGE_OUTPUT.write_text(json.dumps(initial, indent=2) + "\n", encoding="utf-8")

    try:
        coverage = json.loads(COVERAGE_OUTPUT.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"could not read implementation coverage overlay: {error}") from error
    statuses = coverage.get("statuses")
    if not isinstance(statuses, dict):
        raise ValueError("implementation coverage overlay must contain a status mapping")
    recorded_ids = set(statuses)
    missing_ids = sorted(ids - recorded_ids)
    unknown_ids = sorted(recorded_ids - ids)
    if missing_ids or unknown_ids:
        raise ValueError(f"implementation coverage IDs mismatch; missing={missing_ids}; unknown={unknown_ids}")
    invalid = sorted(
        (identifier, status)
        for identifier, status in statuses.items()
        if status not in {"pending", "verified"}
    )
    if invalid:
        raise ValueError(f"implementation coverage contains invalid status values: {invalid}")
    if coverage.get("reference_inventory") != "configuration-parity.json" or coverage.get("reference_commit") != REFERENCE_COMMIT:
        raise ValueError("implementation coverage overlay refers to a different inventory or source commit")


def main() -> None:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument(
        "--check",
        action="store_true",
        help="fail if the checked-in inventory differs from the pinned source inventory",
    )
    arguments = argument_parser.parse_args()
    if subprocess.check_output(["git", "-C", str(REFERENCE_REPO), "rev-parse", REFERENCE_COMMIT], text=True).strip() != REFERENCE_COMMIT:
        raise ValueError("Vibeshine reference commit resolved unexpectedly")

    inventory: dict[tuple[str, str], dict[str, Any]] = {}
    config_schema_keys, client_keys, web_types, defaults, schema_platforms = extract_schema(
        inventory,
        "src_assets/common/assets/web/configs/settingsSchema.ts",
        "src_assets/common/assets/web-legacy/configs/configFieldSchema.ts",
        "src_assets/common/assets/web/configs/extendedSettings.ts",
    )
    docs_text = reference_text("docs/configuration.md")
    heading_domains = docs_headings(docs_text)
    for key, (domain, line_number) in heading_domains.items():
        add_source(inventory, key, "global", "docs/configuration.md", line_number, "documentation_heading", domain)

    parser_keys: set[str] = set()
    parser_types: dict[str, str] = {}
    parser_entries(inventory, "src/config.cpp", parser_keys, parser_types)
    specialized_types = specialized_parser_entries(inventory)
    parser_keys.update(specialized_types)
    host_defaults = specialized_host_defaults()
    for key, (host_type, _host_default, path, line_number) in host_defaults.items():
        parser_types.setdefault(key, host_type)
        add_source(inventory, key, "global", path, line_number, "host_default_declaration")

    global_keys = set(config_schema_keys) | set(heading_domains) | parser_keys
    for key in sorted(global_keys):
        item = inventory.setdefault(("global", key), {"key": key, "scope": "global", "sources": []})
        type_overrides = {
            # int_f stores the converted value, but this converter accepts named
            # configuration tokens rather than integer input.
            "amd_av1_latency_mode": "enum",
            "dd_mode_remapping": "array[display_mode_mapping]",
            "dd_snapshot_exclude_devices": "array[display_device_identifier]",
            "dd_vdd_static_monitor_count": "integer",
            "dd_virtual_display_outputs": "array[display_output_identifier]",
            "dd_snapshot_restore_hotkey": "string",
            "dd_snapshot_restore_hotkey_modifiers": "string",
            "flags": "string",
        }
        type_value = type_overrides.get(key) or parser_types.get(key) or specialized_types.get(key) or web_types.get(key)
        if type_value is not None:
            item["type"] = type_value
        option_defaults: dict[str, Any] = {}
        if key in defaults:
            option_defaults["web_schema"] = defaults[key]
        if key in host_defaults:
            option_defaults["host"] = host_defaults[key][1]
        if option_defaults:
            item["defaults"] = option_defaults
        if len(option_defaults) > 1 and option_defaults["web_schema"] != option_defaults["host"]:
            item["default_conflict"] = {
                "web_schema": option_defaults["web_schema"],
                "host": option_defaults["host"],
            }
        domain = heading_domains.get(key, (None, 0))[0]
        item["domain"] = infer_domain(key, domain, "global")
        item["platform"] = source_platform({source["path"] for source in item["sources"]}, key, schema_platforms)
        override_scope = []
        if key in client_keys:
            override_scope = ["per_application", "per_client"]
        item["override_scopes"] = override_scope
        if key == "flags":
            item["notes"] = "Parser compatibility flag map; it has no direct modern settings field or configuration guide heading."
        if key == "amd_av1_latency_mode":
            item["notes"] = "The host int_f parser uses amd::av1_latency_from_view to accept named enum values; its integer storage does not make the external configuration an integer field."
        if key in {"realtime_stats_history_retention_seconds", "realtime_stats_max_history_points", "realtime_stats_pause_when_hidden", "realtime_stats_show_active_sessions", "realtime_stats_show_host_stats", "realtime_stats_show_host_charts", "realtime_stats_show_session_history"}:
            item["notes"] = "Web-only preference consumed to avoid an unknown-option warning; it is not a native host policy field."
        item["sources"].sort(key=lambda source: (source["path"], source["line"], source["role"]))

    # Runtime app fields live beside the global settings in apps.json and are independently scoped.
    process_text = reference_text("src/process.cpp")
    app_fields = app_entry_keys(process_text)
    app_form_text = reference_text("src_assets/common/assets/web/components/app-edit/types.ts")
    app_form_fields: dict[str, tuple[str, int]] = {}
    lines = app_form_text.splitlines()
    in_server_app = False
    for line_number, line in enumerate(lines, 1):
        if line.startswith("export interface ServerApp"):
            in_server_app = True
        elif in_server_app and line.strip() == "}":
            in_server_app = False
        if in_server_app:
            match = re.match(r"\s*'([^']+)'\??:\s*([^;]+);|\s*([A-Za-z][A-Za-z0-9_]*)\??:\s*([^;]+);", line)
            if match:
                key = match.group(1) or match.group(3)
                type_text = (match.group(2) or match.group(4)).strip()
                app_form_fields[key] = (type_text, line_number)
    for key, value_type, line_number in app_fields:
        item = inventory.setdefault(("per_application", key), {"key": key, "scope": "per_application", "sources": []})
        item["type"] = value_type
        item["domain"] = infer_domain(key, None, "per_application")
        item["platform"] = ["windows"] if key.startswith("playnite-") or key.startswith("gen1-") or key.startswith("gen2-") or key.startswith("dlss-") else None
        add_source(inventory, key, "per_application", "src/process.cpp", line_number, "application_config_parser")
        if key in app_form_fields:
            field_type, form_line = app_form_fields[key]
            add_source(inventory, key, "per_application", "src_assets/common/assets/web/components/app-edit/types.ts", form_line, "application_form_contract", field_type)
        item["sources"].sort(key=lambda source: (source["path"], source["line"], source["role"]))

    for key, (form_type, line_number) in app_form_fields.items():
        if key in {"config-overrides"} or ("-" in key and key not in {field[0] for field in app_fields}):
            continue
        if key not in {field[0] for field in app_fields}:
            continue
        item = inventory.get(("per_application", key))
        if item is not None:
            add_source(inventory, key, "per_application", "src_assets/common/assets/web/components/app-edit/types.ts", line_number, "application_form_contract", form_type)

    # Compare backend authorization and editor discovery lists before emitting the ledger.
    config_cpp = reference_text("src/config.cpp")
    backend_keys, backend_lines = backend_override_keys(config_cpp)
    if backend_keys != client_keys:
        backend_only = sorted(backend_keys - client_keys)
        web_only = sorted(client_keys - backend_keys)
        raise ValueError(f"backend/web override allow-list mismatch; backend-only={backend_only}; web-only={web_only}")
    for key in sorted(backend_keys):
        item = inventory.setdefault(("global", key), {"key": key, "scope": "global", "sources": []})
        add_source(inventory, key, "global", "src/config.cpp", backend_lines[key], "backend_override_allowlist")
        item["override_scopes"] = ["per_application", "per_client"]

    aliases = [
        {
            "alias": "nvenc_force_split_encode",
            "canonical": "nvenc_split_encode",
            "scope": "global",
            "reason": "legacy encoder setting normalized before parsing",
            "source": {"path": "src/config.cpp", "line": 118},
        },
        {
            "alias": "dd_vdd_static_monitor_count",
            "canonical": "dd_virtual_display_permanent_count",
            "scope": "global",
            "reason": "legacy virtual-display count input retained by the parser",
            "source": {"path": "src/config.cpp", "line": 1885},
        },
        {
            "alias": "rtss_disable_vsync_ullm",
            "canonical": "frame_limiter_disable_vsync",
            "scope": "global",
            "reason": "legacy RTSS field maps to the shared frame-limiter VSync setting",
            "source": {"path": "src/config.cpp", "line": 1956},
        },
    ] + app_aliases(process_text)

    # Only parser/document/web declarations can be automatically mapped as global config keys.
    # Missing docs/schema entries remain explicit gaps for parity work rather than silently vanishing.
    options = sorted(inventory.values(), key=lambda item: (item["scope"], item["key"]))
    for item in options:
        item["id"] = f"{item['scope']}:{item['key']}"
        item["sources"].sort(key=lambda source: (source["path"], source["line"], source["role"]))

    global_items = [item for item in options if item["scope"] == "global"]
    missing_domain = [item["key"] for item in options if not item.get("domain")]
    if missing_domain:
        raise ValueError(f"inventory entries without domain classification: {missing_domain}")
    missing_sources = [item["key"] for item in options if not item.get("sources")]
    if missing_sources:
        raise ValueError(f"inventory entries without source references: {missing_sources}")

    parser_without_docs = sorted(parser_keys - set(heading_domains))
    parser_without_web = sorted(parser_keys - config_schema_keys)
    web_without_parser = sorted(config_schema_keys - parser_keys)
    alias_entries = []
    for alias in aliases:
        canonical = inventory.get((alias["scope"], alias["canonical"]))
        if canonical is None:
            raise ValueError(f"alias {alias['alias']} has no canonical inventory entry")
        source = alias["source"]
        alias_entries.append(
            {
                "id": f"alias:{alias['scope']}:{alias['alias']}",
                "alias": alias["alias"],
                "canonical": alias["canonical"],
                "scope": alias["scope"],
                "reason": alias["reason"],
                "type": canonical.get("type"),
                "domain": canonical["domain"],
                "platform": canonical.get("platform"),
                "sources": [{"path": source["path"], "line": source["line"], "role": "legacy_alias"}],
            }
        )
    artifact = {
        "reference": {
            "repository": "../vibeshine",
            "branch": "vibe-test",
            "commit": REFERENCE_COMMIT,
            "meaning": "Configuration surface inventory only; status does not assert behavioral parity.",
        },
        "coverage": {
            "global_options": len(global_items),
            "per_application_fields": sum(item["scope"] == "per_application" for item in options),
            "client_override_keys": len(client_keys),
            "aliases": len(aliases),
            "parser_keys": len(parser_keys),
            "documentation_headings": len(heading_domains),
            "web_schema_keys": len(config_schema_keys),
            "parser_keys_without_documentation_heading": parser_without_docs,
            "parser_keys_without_web_schema_field": parser_without_web,
            "web_schema_keys_without_global_parser": web_without_parser,
            "backend_web_override_lists_match": True,
            "typed_options": sum(item.get("type") is not None for item in options),
            "options_with_ui_default": sum("web_schema" in item.get("defaults", {}) for item in options),
            "options_with_host_default": sum("host" in item.get("defaults", {}) for item in options),
            "default_conflicts": sorted(item["id"] for item in options if item.get("default_conflict")),
            "options_with_platform_classification": sum(item.get("platform") is not None for item in options),
        },
        "aliases": alias_entries,
        "options": options,
    }
    serialized = json.dumps(artifact, ensure_ascii=False, indent=2, sort_keys=False) + "\n"
    implementation_ids = {item["id"] for item in options} | {item["id"] for item in alias_entries}
    validate_implementation_coverage(implementation_ids, initialize=not arguments.check)
    if arguments.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != serialized:
            raise ValueError(f"{OUTPUT} is stale; run python scripts/inventory_configuration.py")
        print(f"Configuration inventory is current: {OUTPUT}")
        print(f"Implementation coverage IDs match: {COVERAGE_OUTPUT}")
        return
    OUTPUT.write_text(serialized, encoding="utf-8")
    print(
        f"Wrote {len(global_items)} global options, "
        f"{artifact['coverage']['per_application_fields']} per-application fields, "
        f"{len(client_keys)} shared override keys, and {len(aliases)} aliases to {OUTPUT}"
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"configuration inventory failed: {error}", file=sys.stderr)
        raise SystemExit(1)
