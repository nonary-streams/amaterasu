#!/usr/bin/env python3
"""Read-only Linux host preflight, baseline snapshot and restoration comparator.

Every native qualification check saves a baseline with this tool before it acts
and compares against it afterwards (contract C8):

    linux_host_snapshot.py preflight [--json]
    linux_host_snapshot.py snapshot --out FILE
    linux_host_snapshot.py compare --baseline FILE [--owned NAME ...]
                                   [--wait-restored SECONDS]

Exit status: 0 clean, 1 drift or not restored, 2 evidence unavailable, malformed
or a usage error.  The module is importable: ``capture_state()`` and
``compare_states(before, after, owned)``.

The tool never mutates the host.  Every external command passes through
``validate_argv``, an allowlist of read-only invocations.  Device nodes are only
inspected with ``stat``, ``getfacl`` and ``os.access``; they are never opened.
It uses only the standard library.

Evidence is fail closed.  A missing tool is recorded as ``unavailable`` with a
reason, two ``unavailable`` readings never count as restored, and malformed
evidence is an error rather than an empty state.  This tool is host evidence
only; it does not establish that any plan milestone passed.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import getpass
import glob
import grp
import ipaddress
import json
import os
import pwd
import re
import shutil
import socket
import struct
import subprocess
import sys
import time
from typing import Callable, Iterable, Sequence

SCHEMA_VERSION = 1

NATIVE_LOCK_PATH = "/home/chasep/sources/amaterasu-targets/native.lock"

# The live desktop.  These outputs and the default sink may never be owned.
PROTECTED_CONNECTORS = ("Virtual-1", "HDMI-A-1")

DEVICE_PATTERNS = (
    "/dev/dri/*",
    "/dev/uhid",
    "/dev/uinput",
    "/dev/vibeshine-ds5",
    "/run/vibeshine/vkms-control.sock",
)
KERNEL_MODULES = ("vibeshine_drm", "vibeshine_ds5", "uhid", "uinput")
DBUS_NAMES = (
    "org.freedesktop.portal.Desktop",
    "org.freedesktop.portal.Documents",
    "org.freedesktop.impl.portal.desktop.kde",
    "org.freedesktop.impl.portal.PermissionStore",
    "org.freedesktop.ReserveDevice1.Audio0",
    "org.freedesktop.ReserveDevice1.Audio1",
    "org.pulseaudio.Server",
)
# Moonlight/GameStream default ports; the live Vibeshine host uses them.
CANDIDATE_PORTS = tuple(range(47984, 48011))
DISK_PATHS = ("/home", "/tmp")
MIN_HOME_FREE_BYTES = 12 * 1024**3
MIN_TMP_FREE_BYTES = 1 * 1024**3
NO_PULSE_OWNER_MODULE = 4294967295
COMMAND_TIMEOUT_SECONDS = 15.0
DBUS_TIMEOUT_SECONDS = 3.0

REQUIRED_SECTIONS = ("displays", "audio", "devices", "processes")
ADVISORY_SECTIONS = (
    "drm_connectors",
    "kernel_modules",
    "dbus_names",
    "nvidia_encoder",
    "vaapi",
    "listeners",
)
INFORMATIONAL_SECTIONS = ("disk", "native_lock", "identity")
ALL_SECTIONS = REQUIRED_SECTIONS + ADVISORY_SECTIONS + INFORMATIONAL_SECTIONS

PROTECTED_PROCESS_GROUPS = ("host", "steam", "game")
HOST_PROGRAM_NAMES = frozenset(
    {"vibeshine-host", "vibeshine-machine-host", "vibeshine-session-controller"}
)
GAME_PROGRAM_NAMES = frozenset(
    {"wineserver", "wine-preloader", "wine64-preloader", "wine", "wine64"}
)
STEAM_RUNTIME_MARKERS = ("pressure-vessel", "SteamLinuxRuntime", "steam-runtime", "srt-")
GAME_COMMAND_MARKERS = ("/steamapps/common/", "SteamLaunch AppId", "mangohud", "/proton ")

# Names an operator may declare as owned.  Nothing else can be excused.
OWNED_PATTERNS = (
    re.compile(r"Virtual-(?:[2-9]|[1-9][0-9]+)"),
    re.compile(r"amaterasu_[A-Za-z0-9_.-]+"),
    re.compile(r"[1-9][0-9]*"),
)

CLASS_RESTORED = "restored"
CLASS_OWNED_GONE = "owned-and-gone"
CLASS_OWNED_PRESENT = "owned-present"
CLASS_DRIFT = "drift"
CLASS_UNVERIFIED = "unverified"
CLASS_INFO = "info"


class EvidenceError(Exception):
    """Evidence is missing or malformed; callers exit with status 2."""


class ToolUnavailable(Exception):
    """A required tool or data source is not present on this host."""


class UsageError(Exception):
    """Invalid command line or ownership declaration; exit status 2."""


# ---------------------------------------------------------------------------
# Read-only command execution
# ---------------------------------------------------------------------------

PACTL_LIST_KINDS = frozenset({"sinks", "sources", "modules", "sink-inputs"})
NVIDIA_QUERY_FIELDS = re.compile(r"--query-gpu=[A-Za-z0-9_.,]+")
STAT_FORMAT = re.compile(r"--printf=(?:%[A-Za-z]|\\[tn]|[^A-Za-z0-9%\\ ])+")
READ_ONLY_PATH = re.compile(r"/(?:dev|run)/[A-Za-z0-9_./-]+")
FORBIDDEN_TOKEN = re.compile(
    r"^(?:output\.|set-|load-|unload-|move-|suspend|kill-|--set|-pm$|-e$|-r$)"
)


def validate_argv(argv: Sequence[str]) -> None:
    """Reject any argv that is not an allowlisted read-only invocation."""
    if not argv or not all(isinstance(token, str) for token in argv):
        raise UsageError(f"refusing malformed command: {argv!r}")
    for token in argv[1:]:
        if FORBIDDEN_TOKEN.match(token):
            raise UsageError(f"refusing mutating token {token!r} in {list(argv)!r}")
    tool = os.path.basename(argv[0])
    arguments = list(argv[1:])
    if _is_allowed_invocation(tool, arguments):
        return
    raise UsageError(f"command is not on the read-only allowlist: {list(argv)!r}")


def _is_allowed_invocation(tool: str, arguments: list[str]) -> bool:
    if tool == "kscreen-doctor":
        return arguments in (["-j"], ["-o"])
    if tool == "pactl":
        if arguments == ["info"]:
            return True
        return (
            len(arguments) == 4
            and arguments[:3] == ["-f", "json", "list"]
            and arguments[3] in PACTL_LIST_KINDS
        )
    if tool == "getfacl":
        return (
            len(arguments) == 2
            and arguments[0] == "-cp"
            and READ_ONLY_PATH.fullmatch(arguments[1]) is not None
        )
    if tool == "stat":
        return (
            len(arguments) == 2
            and STAT_FORMAT.fullmatch(arguments[0]) is not None
            and READ_ONLY_PATH.fullmatch(arguments[1]) is not None
        )
    if tool == "nvidia-smi":
        return (
            len(arguments) == 2
            and NVIDIA_QUERY_FIELDS.fullmatch(arguments[0]) is not None
            and arguments[1] == "--format=csv,noheader,nounits"
        )
    if tool == "vainfo":
        return arguments == []
    return False


@dataclasses.dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: str
    stderr: str = ""


def run_readonly(argv: Sequence[str]) -> CommandResult:
    """Run an allowlisted command with a bounded deadline and no stdin."""
    validate_argv(argv)
    environment = dict(os.environ, LC_ALL="C", LANG="C", NO_COLOR="1")
    try:
        completed = subprocess.run(
            list(argv),
            capture_output=True,
            text=True,
            errors="replace",
            timeout=COMMAND_TIMEOUT_SECONDS,
            stdin=subprocess.DEVNULL,
            env=environment,
            check=False,
        )
    except FileNotFoundError:
        raise ToolUnavailable(f"{argv[0]} not found") from None
    except subprocess.TimeoutExpired:
        raise ToolUnavailable(
            f"{argv[0]} timed out after {COMMAND_TIMEOUT_SECONDS:.0f}s"
        ) from None
    except OSError as error:
        raise ToolUnavailable(f"{argv[0]} could not run: {error}") from None
    return CommandResult(completed.returncode, completed.stdout, completed.stderr)


# ---------------------------------------------------------------------------
# Host access (replaceable in tests)
# ---------------------------------------------------------------------------


class HostProbe:
    """Everything the capture reads from the host, in one replaceable place."""

    proc_root = "/proc"
    drm_sysfs_root = "/sys/class/drm"
    native_lock_path = NATIVE_LOCK_PATH

    def run(self, argv: Sequence[str]) -> CommandResult:
        return run_readonly(argv)

    def which(self, tool: str) -> str | None:
        return shutil.which(tool)

    def glob(self, pattern: str) -> list[str]:
        return sorted(glob.glob(pattern))

    def lexists(self, path: str) -> bool:
        return os.path.lexists(path)

    def read_text(self, path: str) -> str:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return handle.read()

    def list_dir(self, path: str) -> list[str]:
        return sorted(os.listdir(path))

    def access_string(self, path: str) -> str:
        readable = "r" if os.access(path, os.R_OK) else "-"
        writable = "w" if os.access(path, os.W_OK) else "-"
        return readable + writable

    def disk_free_bytes(self, path: str) -> int:
        usage = os.statvfs(path)
        return usage.f_bavail * usage.f_frsize

    def process_owner(self, pid: str) -> str:
        uid = os.stat(f"{self.proc_root}/{pid}").st_uid
        try:
            return pwd.getpwuid(uid).pw_name
        except KeyError:
            return str(uid)

    def process_has_open(self, pid: int, path: str) -> bool | None:
        """Whether ``pid`` has ``path`` open; None when that cannot be read."""
        descriptors = f"{self.proc_root}/{pid}/fd"
        try:
            targets = [os.readlink(f"{descriptors}/{name}") for name in os.listdir(descriptors)]
        except OSError:
            return None
        return os.path.realpath(path) in targets

    def dbus_names_with_owner(self, names: Sequence[str]) -> dict[str, bool]:
        return dbus_name_has_owner(names)

    def hostname(self) -> str:
        return socket.gethostname()

    def now(self) -> float:
        return time.time()

    def own_pid(self) -> int:
        return os.getpid()

    def identity(self) -> dict:
        uid = os.getuid()
        groups = []
        for gid in sorted(set(os.getgroups() + [os.getgid()])):
            try:
                groups.append(grp.getgrgid(gid).gr_name)
            except KeyError:
                groups.append(str(gid))
        return {
            "uid": uid,
            "user": getpass.getuser(),
            "groups": groups,
            "cap_sys_admin": self._effective_cap_sys_admin(),
        }

    def _effective_cap_sys_admin(self) -> bool | None:
        try:
            text = self.read_text(f"{self.proc_root}/self/status")
        except OSError:
            return None
        match = re.search(r"^CapEff:\s*([0-9a-fA-F]+)$", text, re.MULTILINE)
        if not match:
            return None
        return bool(int(match.group(1), 16) & (1 << 21))


# ---------------------------------------------------------------------------
# Minimal D-Bus client: NameHasOwner and nothing else
# ---------------------------------------------------------------------------


def _dbus_pad(buffer: bytearray, alignment: int) -> None:
    buffer.extend(b"\0" * (-len(buffer) % alignment))


def _dbus_string(value: str) -> bytes:
    encoded = value.encode()
    return struct.pack("<I", len(encoded)) + encoded + b"\0"


def _dbus_header_field(fields: bytearray, code: int, signature: str, value: str) -> None:
    _dbus_pad(fields, 8)
    fields.append(code)
    fields.extend(bytes([len(signature)]) + signature.encode() + b"\0")
    if signature == "g":
        fields.extend(bytes([len(value)]) + value.encode() + b"\0")
    else:
        _dbus_pad(fields, 4)
        fields.extend(_dbus_string(value))


def encode_dbus_method_call(
    serial: int, destination: str, path: str, interface: str, member: str, argument: str | None
) -> bytes:
    """Encode a little-endian method call with at most one string argument."""
    fields = bytearray()
    _dbus_header_field(fields, 1, "o", path)
    _dbus_header_field(fields, 2, "s", interface)
    _dbus_header_field(fields, 3, "s", member)
    _dbus_header_field(fields, 6, "s", destination)
    body = b""
    if argument is not None:
        _dbus_header_field(fields, 8, "g", "s")
        body = _dbus_string(argument)
    message = bytearray(b"l\x01\x00\x01")
    message.extend(struct.pack("<II", len(body), serial))
    message.extend(struct.pack("<I", len(fields)))
    message.extend(fields)
    _dbus_pad(message, 8)
    message.extend(body)
    return bytes(message)


def _read_exactly(connection: socket.socket, count: int) -> bytes:
    chunks = []
    remaining = count
    while remaining:
        chunk = connection.recv(remaining)
        if not chunk:
            raise EvidenceError("D-Bus connection closed mid-message")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def decode_dbus_reply(connection: socket.socket) -> tuple[int, int | None, str | None, bytes]:
    """Return (type, reply_serial, error_name, body) for the next message."""
    fixed = _read_exactly(connection, 16)
    if fixed[0:1] != b"l":
        raise EvidenceError("D-Bus peer sent a big-endian message")
    message_type = fixed[1]
    body_length, _serial, fields_length = struct.unpack("<III", fixed[4:16])
    fields = _read_exactly(connection, fields_length)
    _read_exactly(connection, -fields_length % 8)
    body = _read_exactly(connection, body_length)
    reply_serial, error_name = _scan_dbus_fields(fields)
    return message_type, reply_serial, error_name, body


def _scan_dbus_fields(fields: bytes) -> tuple[int | None, str | None]:
    reply_serial = None
    error_name = None
    offset = 0
    while offset < len(fields):
        offset += -offset % 8
        if offset >= len(fields):
            break
        code = fields[offset]
        signature_length = fields[offset + 1]
        signature = fields[offset + 2 : offset + 2 + signature_length].decode()
        offset += 3 + signature_length
        if signature in ("o", "s"):
            offset += -offset % 4
            (length,) = struct.unpack_from("<I", fields, offset)
            text = fields[offset + 4 : offset + 4 + length].decode(errors="replace")
            offset += 4 + length + 1
            if code == 4:
                error_name = text
        elif signature == "g":
            length = fields[offset]
            offset += 2 + length
        elif signature == "u":
            offset += -offset % 4
            (value,) = struct.unpack_from("<I", fields, offset)
            offset += 4
            if code == 5:
                reply_serial = value
        else:
            raise EvidenceError(f"unsupported D-Bus header signature {signature!r}")
    return reply_serial, error_name


def _session_bus_path() -> str:
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS")
    if address:
        for part in address.split(";")[0].removeprefix("unix:").split(","):
            if part.startswith("path="):
                return part.removeprefix("path=")
            if part.startswith("abstract="):
                return "\0" + part.removeprefix("abstract=")
        raise ToolUnavailable(f"unsupported DBUS_SESSION_BUS_ADDRESS {address!r}")
    runtime_dir = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    return f"{runtime_dir}/bus"


def _dbus_call(
    connection: socket.socket, serial: int, destination: str, path: str,
    interface: str, member: str, argument: str | None,
) -> bytes:
    connection.sendall(
        encode_dbus_method_call(serial, destination, path, interface, member, argument)
    )
    while True:
        message_type, reply_serial, error_name, body = decode_dbus_reply(connection)
        if reply_serial != serial:
            continue
        if message_type == 3:
            raise EvidenceError(f"D-Bus error {error_name} from {member}")
        return body


def dbus_name_has_owner(names: Sequence[str]) -> dict[str, bool]:
    """Ask the session bus daemon whether each well-known name has an owner."""
    bus_path = _session_bus_path()
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(DBUS_TIMEOUT_SECONDS)
    try:
        try:
            connection.connect(bus_path)
        except OSError as error:
            raise ToolUnavailable(f"session bus unavailable at {bus_path!r}: {error}") from None
        user_id_hex = str(os.getuid()).encode().hex().encode()
        connection.sendall(b"\0AUTH EXTERNAL " + user_id_hex + b"\r\n")
        authentication = connection.recv(256)
        if not authentication.startswith(b"OK"):
            raise EvidenceError(f"D-Bus authentication refused: {authentication!r}")
        connection.sendall(b"BEGIN\r\n")
        bus = ("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")
        _dbus_call(connection, 1, *bus, "Hello", None)
        owners = {}
        for serial, name in enumerate(names, start=2):
            body = _dbus_call(connection, serial, *bus, "NameHasOwner", name)
            if len(body) != 4:
                raise EvidenceError(f"NameHasOwner({name}) returned {len(body)} bytes")
            owners[name] = bool(struct.unpack("<I", body)[0])
        return owners
    except socket.timeout:
        raise ToolUnavailable("session bus did not answer in time") from None
    finally:
        connection.close()


# ---------------------------------------------------------------------------
# Parsers (pure functions over command output)
# ---------------------------------------------------------------------------

ANSI_PATTERN = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
KSCREEN_OUTPUT_HEADER = re.compile(r"^Output:\s+(\d+)\s+(\S+)")
KSCREEN_MODE_TOKEN = re.compile(r"(\d+):(\d+)x(\d+)@([0-9.]+)(\*?)(!?)")
DISPLAY_FIELDS = (
    "connected", "enabled", "mode", "refresh_hz", "position", "priority",
    "scale", "rotation", "hdr", "wcg", "replication_source",
)


def _require_mapping(value: object, what: str) -> dict:
    if not isinstance(value, dict):
        raise EvidenceError(f"{what} is not an object")
    return value


def _require_list(value: object, what: str) -> list:
    if not isinstance(value, list):
        raise EvidenceError(f"{what} is not a list")
    return value


def _round_refresh(value: float) -> float:
    return round(float(value), 2)


def parse_kscreen_json(text: str) -> dict[str, dict]:
    """Normalize ``kscreen-doctor -j`` into connector name -> meaningful state."""
    try:
        document = json.loads(text)
    except ValueError as error:
        raise EvidenceError(f"kscreen-doctor -j is not JSON: {error}") from None
    outputs = _require_mapping(document, "kscreen document").get("outputs")
    if not isinstance(outputs, list) or not outputs:
        raise EvidenceError("kscreen-doctor -j reported no outputs")
    state: dict[str, dict] = {}
    for raw in outputs:
        output = _normalize_kscreen_json_output(_require_mapping(raw, "kscreen output"))
        name = output.pop("name")
        if name in state:
            raise EvidenceError(f"kscreen reported {name} twice")
        state[name] = output
    return state


def _normalize_kscreen_json_output(raw: dict) -> dict:
    for key in ("name", "connected", "enabled"):
        if key not in raw:
            raise EvidenceError(f"kscreen output is missing {key!r}")
    modes = _require_list(raw.get("modes", []), "kscreen modes")
    current = next(
        (mode for mode in modes if str(mode.get("id")) == str(raw.get("currentModeId"))),
        None,
    )
    if current is None and raw["enabled"]:
        raise EvidenceError(f"enabled output {raw['name']} has no current mode")
    position = _require_mapping(raw.get("pos", {}), "kscreen pos")
    if raw["enabled"] and not {"x", "y"} <= position.keys():
        raise EvidenceError(f"enabled output {raw['name']} has no position")
    return {
        "name": str(raw["name"]),
        "connected": bool(raw["connected"]),
        "enabled": bool(raw["enabled"]),
        "mode": _mode_size(current),
        "refresh_hz": _round_refresh(current["refreshRate"]) if current else None,
        "position": [position.get("x"), position.get("y")],
        "priority": raw.get("priority"),
        "scale": _round_scale(raw.get("scale")),
        "rotation": raw.get("rotation"),
        "hdr": raw.get("hdr"),
        "wcg": raw.get("wcg"),
        "replication_source": raw.get("replicationSource"),
    }


def _mode_size(mode: dict | None) -> str | None:
    if mode is None:
        return None
    size = _require_mapping(mode.get("size"), "kscreen mode size")
    return f"{size['width']}x{size['height']}"


def _round_scale(value: object) -> float | None:
    return None if value is None else round(float(value), 3)


def parse_kscreen_text(text: str) -> dict[str, dict]:
    """Normalize ``kscreen-doctor -o`` (the fallback for older KScreen)."""
    clean = ANSI_PATTERN.sub("", text)
    state: dict[str, dict] = {}
    current: dict | None = None
    for raw_line in clean.splitlines():
        line = raw_line.strip()
        header = KSCREEN_OUTPUT_HEADER.match(line)
        if header:
            current = _blank_text_output()
            if header.group(2) in state:
                raise EvidenceError(f"kscreen reported {header.group(2)} twice")
            state[header.group(2)] = current
        elif current is not None:
            _apply_kscreen_text_line(current, line)
    if not state:
        raise EvidenceError("kscreen-doctor -o reported no outputs")
    for name, output in state.items():
        _finish_text_output(name, output)
    return state


def _blank_text_output() -> dict:
    output = dict.fromkeys(DISPLAY_FIELDS)
    output["hdr"] = False
    output["wcg"] = False
    return output


def _apply_kscreen_text_line(output: dict, line: str) -> None:
    simple_flags = {
        "enabled": ("enabled", True), "disabled": ("enabled", False),
        "connected": ("connected", True), "disconnected": ("connected", False),
    }
    if line in simple_flags:
        key, value = simple_flags[line]
        output[key] = value
        return
    if line.startswith("Modes:"):
        _apply_kscreen_modes(output, line)
        return
    patterns = (
        (r"priority (\d+)", "priority", int),
        (r"replication source:(\d+)", "replication_source", int),
        (r"Scale: ([0-9.]+)", "scale", lambda value: round(float(value), 3)),
        (r"Rotation: (\d+)", "rotation", int),
        (r"HDR: (enabled|disabled)", "hdr", lambda value: value == "enabled"),
        (r"Wide Color Gamut: (enabled|disabled)", "wcg", lambda value: value == "enabled"),
    )
    for pattern, key, convert in patterns:
        match = re.fullmatch(pattern, line)
        if match:
            output[key] = convert(match.group(1))
            return
    geometry = re.fullmatch(r"Geometry: (-?\d+),(-?\d+) \d+x\d+", line)
    if geometry:
        output["position"] = [int(geometry.group(1)), int(geometry.group(2))]


def _apply_kscreen_modes(output: dict, line: str) -> None:
    for _mode_id, width, height, refresh, current, _preferred in KSCREEN_MODE_TOKEN.findall(line):
        if current:
            output["mode"] = f"{width}x{height}"
            output["refresh_hz"] = _round_refresh(refresh)


def _finish_text_output(name: str, output: dict) -> None:
    if output["enabled"] is None or output["connected"] is None:
        raise EvidenceError(f"kscreen text for {name} lacks enabled/connected state")
    if output["enabled"] and (output["mode"] is None or output["position"] is None):
        raise EvidenceError(f"enabled output {name} lacks a current mode or position")
    if output["position"] is None:
        output["position"] = [None, None]


def parse_pactl_info(text: str) -> dict[str, str]:
    """Parse ``pactl info`` (C locale) into key -> value."""
    info = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            info[key.strip()] = value.strip()
    for key in ("Server Name", "Default Sink", "Default Source"):
        if key not in info:
            raise EvidenceError(f"pactl info lacks {key!r}")
    return info


def parse_pactl_json_list(text: str, what: str) -> list[dict]:
    try:
        document = json.loads(text)
    except ValueError as error:
        raise EvidenceError(f"pactl list {what} is not JSON: {error}") from None
    entries = _require_list(document, f"pactl list {what}")
    return [_require_mapping(entry, f"pactl {what} entry") for entry in entries]


def normalize_sinks(entries: list[dict]) -> dict[str, dict]:
    sinks = {}
    for entry in entries:
        name = _require_name(entry, "sink")
        sinks[name] = {
            "description": entry.get("description"),
            "driver": entry.get("driver"),
            "sample_spec": entry.get("sample_specification"),
            "channel_map": entry.get("channel_map"),
            "monitor_source": entry.get("monitor_source"),
            "module_owned": entry.get("owner_module") not in (None, NO_PULSE_OWNER_MODULE),
        }
    return sinks


def normalize_sources(entries: list[dict]) -> dict[str, dict]:
    sources = {}
    for entry in entries:
        name = _require_name(entry, "source")
        sources[name] = {
            "description": entry.get("description"),
            "driver": entry.get("driver"),
            "sample_spec": entry.get("sample_specification"),
            "channel_map": entry.get("channel_map"),
            "module_owned": entry.get("owner_module") not in (None, NO_PULSE_OWNER_MODULE),
        }
    return sources


def normalize_modules(entries: list[dict]) -> list[dict]:
    modules = [
        {"name": _require_name(entry, "module"), "argument": " ".join(str(entry.get("argument") or "").split())}
        for entry in entries
    ]
    return sorted(modules, key=lambda module: (module["name"], module["argument"]))


def normalize_sink_inputs(entries: list[dict], sinks_by_index: dict[int, str]) -> dict[str, dict]:
    inputs = {}
    for entry in entries:
        if "index" not in entry:
            raise EvidenceError("sink-input lacks an index")
        properties = entry.get("properties") or {}
        sink_index = entry.get("sink")
        inputs[str(entry["index"])] = {
            "application": properties.get("application.name"),
            "binary": properties.get("application.process.binary"),
            "pid": properties.get("application.process.id"),
            "media": properties.get("media.name"),
            "sink": sinks_by_index.get(sink_index, f"unknown-sink-{sink_index}"),
        }
    return inputs


def _require_name(entry: dict, what: str) -> str:
    name = entry.get("name")
    if not isinstance(name, str) or not name:
        raise EvidenceError(f"{what} entry has no name")
    return name


def parse_stat_line(output: str, path: str) -> dict:
    fields = output.rstrip("\n").split("\t")
    if len(fields) != 5 or not all(fields[:4]):
        raise EvidenceError(f"stat {path} returned an unexpected record: {output!r}")
    file_type, mode, owner, group, rdev = fields
    return {"type": file_type, "mode": mode, "owner": owner, "group": group, "rdev": rdev}


def parse_getfacl(output: str, path: str) -> list[str]:
    lines = sorted(line.strip() for line in output.splitlines() if line.strip())
    if not any(line.startswith("user::") for line in lines):
        raise EvidenceError(f"getfacl {path} returned no base entries")
    return lines


def parse_proc_modules(text: str) -> dict[str, str]:
    modules = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 5:
            modules[fields[0]] = fields[4]
    return modules


def parse_nvidia_encoder_sessions(text: str) -> list[dict]:
    gpus = []
    for line in text.splitlines():
        if not line.strip():
            continue
        fields = [field.strip() for field in line.split(",")]
        if len(fields) != 4 or not fields[3].isdigit():
            raise EvidenceError(f"unexpected nvidia-smi record: {line!r}")
        gpus.append(
            {"index": fields[0], "uuid": fields[1], "name": fields[2], "encoder_sessions": int(fields[3])}
        )
    if not gpus:
        raise EvidenceError("nvidia-smi reported no GPUs")
    return gpus


VAAPI_ENTRY = re.compile(r"^\s*(VAProfile\w+)\s*:\s*(VAEntrypoint\w+)\s*$")


def parse_vainfo(text: str) -> dict:
    entries = sorted(
        f"{match.group(1)}:{match.group(2)}"
        for match in map(VAAPI_ENTRY.match, text.splitlines())
        if match
    )
    if not entries:
        raise EvidenceError("vainfo listed no profiles")
    driver = next(
        (line.split(":", 1)[1].strip() for line in text.splitlines() if "Driver version" in line),
        None,
    )
    return {"driver": driver, "entries": entries}


def decode_proc_net_address(encoded: str) -> tuple[str, int]:
    address_hex, port_hex = encoded.split(":")
    raw = bytes.fromhex(address_hex)
    if len(raw) == 4:
        address = ipaddress.IPv4Address(raw[::-1])
    elif len(raw) == 16:
        words = [raw[index : index + 4][::-1] for index in range(0, 16, 4)]
        address = ipaddress.IPv6Address(b"".join(words))
    else:
        raise EvidenceError(f"bad /proc/net address {encoded!r}")
    return str(address), int(port_hex, 16)


def parse_proc_net_listeners(text: str, protocol: str, ports: frozenset[int]) -> list[dict]:
    listeners = []
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 4:
            continue
        if protocol.startswith("tcp") and fields[3] != "0A":
            continue
        address, port = decode_proc_net_address(fields[1])
        if port in ports:
            listeners.append({"proto": protocol, "address": address, "port": port})
    return listeners


def parse_proc_stat(text: str) -> tuple[str, str, str]:
    """Return (comm, ppid, starttime) from /proc/<pid>/stat."""
    close = text.rfind(")")
    open_ = text.find("(")
    if open_ < 0 or close < open_:
        raise EvidenceError("malformed /proc stat record")
    fields = text[close + 2 :].split()
    if len(fields) < 20:
        raise EvidenceError("short /proc stat record")
    return text[open_ + 1 : close], fields[1], fields[19]


def classify_process(argv: list[str]) -> str | None:
    """Group a command line, or None when the process is not tracked."""
    programs = [os.path.basename(token) for token in argv[:3]]
    if any(program in HOST_PROGRAM_NAMES for program in programs):
        return "host"
    if any(program.startswith("vibeshine-") for program in programs):
        return "session_helper"
    if programs and programs[0] == "steam":
        return "steam"
    command_line = " ".join(argv)
    if any(marker in command_line for marker in STEAM_RUNTIME_MARKERS):
        return None  # Steam's own container plumbing churns; it is not a game
    if programs and programs[0] in GAME_PROGRAM_NAMES:
        return "game"
    if any(marker in command_line for marker in GAME_COMMAND_MARKERS):
        return "game"
    return None


# ---------------------------------------------------------------------------
# Section capture
# ---------------------------------------------------------------------------


def capture_displays(probe: HostProbe) -> dict:
    json_result = _try_kscreen(probe, ["kscreen-doctor", "-j"])
    if json_result is not None:
        return {"source": "kscreen-doctor -j", "outputs": parse_kscreen_json(json_result)}
    text_result = _try_kscreen(probe, ["kscreen-doctor", "-o"])
    if text_result is None:
        raise ToolUnavailable("kscreen-doctor -j and -o both failed")
    return {"source": "kscreen-doctor -o", "outputs": parse_kscreen_text(text_result)}


def _try_kscreen(probe: HostProbe, argv: list[str]) -> str | None:
    result = probe.run(argv)
    if result.returncode != 0 or not result.stdout.strip():
        return None
    return result.stdout


def capture_audio(probe: HostProbe) -> dict:
    info = _pactl(probe, ["pactl", "info"], "info")
    parsed_info = parse_pactl_info(info)
    listed = {
        kind: parse_pactl_json_list(_pactl(probe, ["pactl", "-f", "json", "list", kind], kind), kind)
        for kind in ("sinks", "sources", "modules", "sink-inputs")
    }
    sinks_by_index = {
        entry["index"]: entry["name"] for entry in listed["sinks"] if "index" in entry and "name" in entry
    }
    return {
        "server": parsed_info["Server Name"],
        "default_sink": parsed_info["Default Sink"],
        "default_source": parsed_info["Default Source"],
        "sinks": normalize_sinks(listed["sinks"]),
        "sources": normalize_sources(listed["sources"]),
        "modules": normalize_modules(listed["modules"]),
        "sink_inputs": normalize_sink_inputs(listed["sink-inputs"], sinks_by_index),
    }


def _pactl(probe: HostProbe, argv: list[str], what: str) -> str:
    result = probe.run(argv)
    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()[:1]
        raise ToolUnavailable(f"pactl {what} failed ({result.returncode}) {' '.join(detail)}".strip())
    return result.stdout


def capture_devices(probe: HostProbe) -> dict:
    paths: set[str] = set()
    for pattern in DEVICE_PATTERNS:
        paths.update(probe.glob(pattern) if "*" in pattern else [pattern])
    return {"entries": {path: _capture_device(probe, path) for path in sorted(paths)}}


def _capture_device(probe: HostProbe, path: str) -> dict:
    if not probe.lexists(path):
        return {"exists": False}
    stat_result = probe.run(["stat", r"--printf=%F\t%a\t%U\t%G\t%t:%T\n", path])
    if stat_result.returncode != 0:
        raise ToolUnavailable(f"stat {path} failed: {stat_result.stderr.strip()}")
    entry = {"exists": True, **parse_stat_line(stat_result.stdout, path)}
    acl_result = probe.run(["getfacl", "-cp", path])
    if acl_result.returncode != 0:
        raise ToolUnavailable(f"getfacl {path} failed: {acl_result.stderr.strip()}")
    entry["acl"] = parse_getfacl(acl_result.stdout, path)
    entry["access"] = probe.access_string(path)
    return entry


def capture_kernel_modules(probe: HostProbe) -> dict:
    loaded = parse_proc_modules(probe.read_text(f"{probe.proc_root}/modules"))
    return {
        "modules": {
            name: {"loaded": name in loaded, "state": loaded.get(name)} for name in KERNEL_MODULES
        }
    }


def capture_drm_connectors(probe: HostProbe) -> dict:
    connectors = {}
    for entry in probe.list_dir(probe.drm_sysfs_root):
        if re.fullmatch(r"card\d+-.+", entry):
            status = probe.read_text(f"{probe.drm_sysfs_root}/{entry}/status").strip()
            if status not in ("connected", "disconnected", "unknown"):
                raise EvidenceError(f"{entry} reports status {status!r}")
            connectors[entry] = {"status": status}
    if not connectors:
        raise EvidenceError("no DRM connectors found")
    return {"connectors": connectors}


def capture_dbus_names(probe: HostProbe) -> dict:
    return {"names": probe.dbus_names_with_owner(DBUS_NAMES)}


def capture_nvidia_encoder(probe: HostProbe) -> dict:
    if probe.which("nvidia-smi") is None:
        raise ToolUnavailable("nvidia-smi not found")
    query = "--query-gpu=index,uuid,name,encoder.stats.sessionCount"
    result = probe.run(["nvidia-smi", query, "--format=csv,noheader,nounits"])
    if result.returncode != 0:
        raise ToolUnavailable(f"nvidia-smi query failed ({result.returncode})")
    return {"gpus": parse_nvidia_encoder_sessions(result.stdout)}


def capture_vaapi(probe: HostProbe) -> dict:
    if probe.which("vainfo") is None:
        raise ToolUnavailable("vainfo not installed (no VAAPI tools)")
    result = probe.run(["vainfo"])
    if result.returncode != 0:
        raise ToolUnavailable(f"vainfo failed ({result.returncode})")
    return parse_vainfo(result.stdout)


def capture_listeners(probe: HostProbe, extra_ports: Iterable[int] = ()) -> dict:
    ports = frozenset(CANDIDATE_PORTS) | frozenset(extra_ports)
    listeners = []
    for protocol in ("tcp", "tcp6", "udp", "udp6"):
        path = f"{probe.proc_root}/net/{protocol}"
        try:
            text = probe.read_text(path)
        except FileNotFoundError:
            if protocol in ("tcp", "udp"):
                raise
            continue
        listeners.extend(parse_proc_net_listeners(text, protocol, ports))
    return {"ports": sorted(ports), "listeners": sorted(listeners, key=_listener_key)}


def _listener_key(listener: dict) -> tuple:
    return (listener["proto"], listener["port"], listener["address"])


def capture_processes(probe: HostProbe) -> dict:
    processes = []
    for entry in probe.list_dir(probe.proc_root):
        if entry.isdigit():
            record = _capture_process(probe, entry)
            if record is not None:
                processes.append(record)
    return {"processes": sorted(processes, key=lambda record: record["pid"])}


def _capture_process(probe: HostProbe, pid: str) -> dict | None:
    try:
        comm, ppid, starttime = parse_proc_stat(probe.read_text(f"{probe.proc_root}/{pid}/stat"))
        cmdline = probe.read_text(f"{probe.proc_root}/{pid}/cmdline")
        owner = probe.process_owner(pid)
    except (OSError, EvidenceError):
        return None  # the process exited while it was being read
    argv = [token for token in cmdline.split("\0") if token]
    group = classify_process(argv)
    if group is None:
        return None
    # Only program names are kept: arguments may carry tokens or credentials.
    return {
        "pid": int(pid), "starttime": starttime, "ppid": int(ppid), "comm": comm,
        "user": owner, "group": group,
        "program": [os.path.basename(token) for token in argv[:3]],
    }


def capture_disk(probe: HostProbe) -> dict:
    return {"free_bytes": {path: probe.disk_free_bytes(path) for path in DISK_PATHS}}


def capture_native_lock(probe: HostProbe) -> dict:
    """Report who holds the native lock from /proc/locks without locking it."""
    path = probe.native_lock_path
    try:
        status = os.stat(path)
    except FileNotFoundError:
        return {"path": path, "state": "absent", "holder_pid": None}
    device = f"{os.major(status.st_dev):02x}:{os.minor(status.st_dev):02x}"
    holders = _flock_holders(
        probe.read_text(f"{probe.proc_root}/locks"), device, status.st_ino,
        lambda pid: probe.process_has_open(pid, path),
    )
    if not holders:
        return {"path": path, "state": "free", "holder_pid": None}
    ours = _process_ancestry(probe, probe.own_pid())
    state = "held_by_this_process_tree" if ours & set(holders) else "held_by_other"
    return {"path": path, "state": state, "holder_pid": holders[0]}


def _flock_holders(
    locks_text: str, device: str, inode: int, holds_file: Callable[[int], bool | None]
) -> list[int]:
    """Find FLOCK holders of ``inode``.

    /proc/locks reports the superblock device, which on btrfs differs from the
    subvolume device stat() returns, so an inode match with another device is
    accepted unless the holder is readable and demonstrably does not have the
    file open.  Missing a real holder would be unsafe; a false holder is not.
    """
    holders = []
    for line in locks_text.splitlines():
        fields = line.split()
        if len(fields) < 6 or fields[1] != "FLOCK":
            continue
        lock_device, _, lock_inode = fields[5].rpartition(":")
        if lock_inode != str(inode):
            continue
        pid = int(fields[4])
        if lock_device == device or holds_file(pid) is not False:
            holders.append(pid)
    return holders


def _process_ancestry(probe: HostProbe, pid: int) -> set[int]:
    chain = set()
    while pid > 1 and pid not in chain:
        chain.add(pid)
        try:
            _comm, ppid, _start = parse_proc_stat(probe.read_text(f"{probe.proc_root}/{pid}/stat"))
        except (OSError, EvidenceError):
            break
        pid = int(ppid)
    return chain


def capture_identity(probe: HostProbe) -> dict:
    return probe.identity()


def reading_ok(data: dict) -> dict:
    return {"status": "ok", "data": data}


def reading_unavailable(reason: str) -> dict:
    return {"status": "unavailable", "reason": reason}


def capture_section(capture: Callable[[], dict]) -> dict:
    """Run one capture, converting every failure into an ``unavailable`` reading."""
    try:
        return reading_ok(capture())
    except ToolUnavailable as error:
        return reading_unavailable(str(error))
    except EvidenceError as error:
        return reading_unavailable(f"malformed evidence: {error}")
    except UsageError:
        raise
    except OSError as error:
        return reading_unavailable(f"{type(error).__name__}: {error}")


def capture_state(probe: HostProbe | None = None, extra_ports: Iterable[int] = ()) -> dict:
    """Read the whole host state.  Never mutates anything."""
    probe = probe or HostProbe()
    captures: dict[str, Callable[[], dict]] = {
        "displays": lambda: capture_displays(probe),
        "audio": lambda: capture_audio(probe),
        "devices": lambda: capture_devices(probe),
        "processes": lambda: capture_processes(probe),
        "drm_connectors": lambda: capture_drm_connectors(probe),
        "kernel_modules": lambda: capture_kernel_modules(probe),
        "dbus_names": lambda: capture_dbus_names(probe),
        "nvidia_encoder": lambda: capture_nvidia_encoder(probe),
        "vaapi": lambda: capture_vaapi(probe),
        "listeners": lambda: capture_listeners(probe, extra_ports),
        "disk": lambda: capture_disk(probe),
        "native_lock": lambda: capture_native_lock(probe),
        "identity": lambda: capture_identity(probe),
    }
    now = probe.now()
    return {
        "schema_version": SCHEMA_VERSION,
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now)),
        "captured_epoch": now,
        "host": probe.hostname(),
        "boot_id": _boot_id(probe),
        "sections": {name: capture_section(capture) for name, capture in captures.items()},
    }


def _boot_id(probe: HostProbe) -> str | None:
    try:
        return probe.read_text(f"{probe.proc_root}/sys/kernel/random/boot_id").strip()
    except OSError:
        return None


# ---------------------------------------------------------------------------
# State validation, private persistence
# ---------------------------------------------------------------------------

SECTION_DATA_KEYS = {
    "displays": ("outputs",),
    "audio": ("default_sink", "default_source", "sinks", "sources", "modules", "sink_inputs"),
    "devices": ("entries",),
    "processes": ("processes",),
    "drm_connectors": ("connectors",),
    "kernel_modules": ("modules",),
    "dbus_names": ("names",),
    "nvidia_encoder": ("gpus",),
    "listeners": ("listeners",),
    "disk": ("free_bytes",),
    "native_lock": ("state",),
    "identity": ("uid",),
}


def validate_state(state: object, label: str = "state") -> dict:
    """Return ``state`` if structurally sound, otherwise raise EvidenceError."""
    document = _require_mapping(state, label)
    if document.get("schema_version") != SCHEMA_VERSION:
        raise EvidenceError(f"{label}: unsupported schema_version {document.get('schema_version')!r}")
    sections = _require_mapping(document.get("sections"), f"{label} sections")
    for name in ALL_SECTIONS:
        reading = sections.get(name)
        if not isinstance(reading, dict):
            raise EvidenceError(f"{label}: section {name!r} is missing")
        _validate_reading(label, name, reading)
    return document


def _validate_reading(label: str, name: str, reading: dict) -> None:
    status = reading.get("status")
    if status == "unavailable":
        reason = reading.get("reason")
        if not isinstance(reason, str) or not reason:
            raise EvidenceError(f"{label}: unavailable section {name!r} gives no reason")
        return
    if status != "ok":
        raise EvidenceError(f"{label}: section {name!r} has status {status!r}")
    data = reading.get("data")
    if not isinstance(data, dict):
        raise EvidenceError(f"{label}: section {name!r} has no data object")
    for key in SECTION_DATA_KEYS.get(name, ()):
        if key not in data:
            raise EvidenceError(f"{label}: section {name!r} lacks {key!r}")
    _validate_item_shapes(label, name, data)


ITEM_SHAPES = {
    # section -> (collection key, required fields of every item)
    "displays": ("outputs", DISPLAY_FIELDS),
    "devices": ("entries", ("exists",)),
    "processes": ("processes", ("pid", "starttime", "group")),
    "drm_connectors": ("connectors", ("status",)),
    "audio": ("sinks", ("module_owned", "monitor_source")),
}


def _validate_item_shapes(label: str, name: str, data: dict) -> None:
    if name not in ITEM_SHAPES:
        return
    collection, fields = ITEM_SHAPES[name]
    items = data[collection]
    entries = items.items() if isinstance(items, dict) else enumerate(items)
    if not isinstance(items, (dict, list)) or (name == "displays" and not items):
        raise EvidenceError(f"{label}: {name}.{collection} is empty or not a collection")
    for key, item in entries:
        if not isinstance(item, dict) or not all(field in item for field in fields):
            raise EvidenceError(f"{label}: {name}.{collection}[{key!r}] is malformed")
    if name == "audio":
        for field in ("sources", "sink_inputs"):
            if not isinstance(data[field], dict) or not isinstance(data["modules"], list):
                raise EvidenceError(f"{label}: audio.{field} is malformed")
        if not isinstance(data["default_sink"], str):
            raise EvidenceError(f"{label}: audio.default_sink is malformed")


def unavailable_required_sections(state: dict) -> list[tuple[str, str]]:
    return [
        (name, state["sections"][name]["reason"])
        for name in REQUIRED_SECTIONS
        if state["sections"][name]["status"] != "ok"
    ]


def write_private_json(path: str, document: dict) -> None:
    """Write ``document`` to a new 0600 file inside a 0700 directory."""
    directory = os.path.dirname(os.path.abspath(path))
    _ensure_private_directory(directory)
    temporary = os.path.join(directory, f".{os.path.basename(path)}.{os.getpid()}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(document, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temporary, path)
        except FileExistsError:
            raise UsageError(f"{path} already exists; every baseline is a new file") from None
    finally:
        os.unlink(temporary)
    os.chmod(path, 0o600)


def _ensure_private_directory(directory: str) -> None:
    if not os.path.isdir(directory):
        os.makedirs(os.path.dirname(directory), exist_ok=True)
        os.mkdir(directory, 0o700)
        os.chmod(directory, 0o700)
        return
    status = os.stat(directory)
    if status.st_uid != os.getuid() or status.st_mode & 0o077:
        raise UsageError(
            f"{directory} exists but is not a private (0700) directory owned by you; "
            "choose a new directory so the baseline stays private"
        )


def load_state_file(path: str) -> dict:
    try:
        with open(path, encoding="utf-8") as handle:
            document = json.load(handle)
    except FileNotFoundError:
        raise EvidenceError(f"baseline {path} does not exist") from None
    except (OSError, ValueError) as error:
        raise EvidenceError(f"baseline {path} is unreadable or malformed: {error}") from None
    return validate_state(document, f"baseline {path}")


# ---------------------------------------------------------------------------
# Owned-resource declarations
# ---------------------------------------------------------------------------


def validate_owned_names(owned: Iterable[str], baseline: dict | None = None) -> frozenset[str]:
    """Accept only names an operator may own; never the live desktop or sinks."""
    names = frozenset(owned)
    protected = set(PROTECTED_CONNECTORS)
    if baseline is not None:
        audio = baseline["sections"]["audio"]
        if audio["status"] == "ok":
            protected.update([audio["data"]["default_sink"], audio["data"]["default_source"]])
    for name in sorted(names):
        if name in protected:
            raise UsageError(f"{name} is the live desktop or default audio and cannot be owned")
        if not any(pattern.fullmatch(name) for pattern in OWNED_PATTERNS):
            raise UsageError(
                f"{name!r} cannot be declared owned: use Virtual-2 or higher, "
                "an amaterasu_<name> audio sink, or a PID"
            )
    return names


# ---------------------------------------------------------------------------
# Comparison
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Finding:
    area: str
    key: str
    classification: str
    detail: str
    blocking: bool = False
    before: object = None
    after: object = None

    def as_dict(self) -> dict:
        return dataclasses.asdict(self)


@dataclasses.dataclass
class Comparison:
    findings: list[Finding]

    @property
    def exit_code(self) -> int:
        if any(f.blocking and f.classification in (CLASS_DRIFT, CLASS_OWNED_PRESENT) for f in self.findings):
            return 1
        if any(f.blocking and f.classification == CLASS_UNVERIFIED for f in self.findings):
            return 2
        return 0

    @property
    def clean(self) -> bool:
        return self.exit_code == 0

    def by_class(self, classification: str) -> list[Finding]:
        return [f for f in self.findings if f.classification == classification]


def describe_changes(before: object, after: object) -> str:
    if isinstance(before, dict) and isinstance(after, dict):
        keys = sorted(set(before) | set(after))
        changes = [f"{key}: {before.get(key)!r} -> {after.get(key)!r}" for key in keys if before.get(key) != after.get(key)]
        return "; ".join(changes)
    return f"{before!r} -> {after!r}"


@dataclasses.dataclass
class AreaComparer:
    """Compares keyed items of one area, applying the owned-resource rules."""

    area: str
    is_owned: Callable[[str, object, object], bool]
    allow_owned_present: bool
    protected: frozenset[str] = frozenset()
    findings: list[Finding] = dataclasses.field(default_factory=list)

    def compare_items(self, before: dict, after: dict) -> list[Finding]:
        for key in sorted(set(before) | set(after)):
            self._compare_item(key, before.get(key), after.get(key), key in before, key in after)
        return self.findings

    def _compare_item(self, key: str, old: object, new: object, in_before: bool, in_after: bool) -> None:
        owned = self.is_owned(key, old, new)
        if in_before and in_after and old == new:
            self._add(key, CLASS_RESTORED, "matches baseline", False, old, new)
        elif owned and in_before and not in_after:
            self._add(key, CLASS_OWNED_GONE, "owned resource is gone", False, old, None)
        elif owned:
            self._add(
                key, CLASS_OWNED_PRESENT, f"owned resource still present ({describe_changes(old, new)})",
                not self.allow_owned_present, old, new,
            )
        else:
            self._add(key, CLASS_DRIFT, self._drift_detail(key, old, new, in_before, in_after), True, old, new)

    def _drift_detail(self, key: str, old: object, new: object, in_before: bool, in_after: bool) -> str:
        label = "protected " if key in self.protected else ""
        if in_before and not in_after:
            return f"{label}item disappeared since the baseline"
        if in_after and not in_before:
            return f"{label}item appeared since the baseline"
        return f"{label}item changed: {describe_changes(old, new)}"

    def _add(self, key: str, classification: str, detail: str, blocking: bool, old: object, new: object) -> None:
        self.findings.append(Finding(self.area, key, classification, detail, blocking, old, new))


def _scalar_finding(area: str, key: str, before: object, after: object) -> Finding:
    if before == after:
        return Finding(area, key, CLASS_RESTORED, f"{before!r} unchanged", False, before, after)
    return Finding(area, key, CLASS_DRIFT, f"changed: {before!r} -> {after!r}", True, before, after)


def _never_owned(_key: str, _old: object, _new: object) -> bool:
    return False


def _owned_by_key(owned: frozenset[str]) -> Callable[[str, object, object], bool]:
    return lambda key, _old, _new: key in owned


def compare_displays(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    comparer = AreaComparer("displays", _owned_by_key(owned), allow_present, frozenset(PROTECTED_CONNECTORS))
    return comparer.compare_items(before["outputs"], after["outputs"])


def compare_drm_connectors(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    def connector_owned(key: str, _old: object, _new: object) -> bool:
        return key.split("-", 1)[-1] in owned

    def existing(connectors: dict) -> dict:
        # An owned connector slot always exists in sysfs; only "connected" is a live output.
        return {
            key: value for key, value in connectors.items()
            if not (connector_owned(key, None, None) and value["status"] != "connected")
        }

    comparer = AreaComparer("drm_connectors", connector_owned, allow_present)
    return comparer.compare_items(existing(before["connectors"]), existing(after["connectors"]))


def compare_devices(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    return AreaComparer("devices", _owned_by_key(owned), allow_present).compare_items(
        before["entries"], after["entries"]
    )


def compare_kernel_modules(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    return AreaComparer("kernel_modules", _never_owned, allow_present).compare_items(
        before["modules"], after["modules"]
    )


def compare_dbus_names(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    return AreaComparer("dbus_names", _never_owned, allow_present).compare_items(
        before["names"], after["names"]
    )


def compare_nvidia_encoder(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    def by_uuid(data: dict) -> dict:
        return {gpu["uuid"]: {"encoder_sessions": gpu["encoder_sessions"], "name": gpu["name"]} for gpu in data["gpus"]}

    return AreaComparer("nvidia_encoder", _never_owned, allow_present).compare_items(
        by_uuid(before), by_uuid(after)
    )


def compare_vaapi(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    return AreaComparer("vaapi", _never_owned, allow_present).compare_items(
        {"profiles": before}, {"profiles": after}
    )


def compare_listeners(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    def keyed(data: dict) -> dict:
        return {f"{item['proto']}/{item['address']}:{item['port']}": item for item in data["listeners"]}

    return AreaComparer("listeners", _never_owned, allow_present).compare_items(keyed(before), keyed(after))


def _sink_owner_matcher(owned: frozenset[str]) -> Callable[[str, object, object], bool]:
    def source_owned(name: str, _old: object, _new: object) -> bool:
        return name in owned or (name.endswith(".monitor") and name[: -len(".monitor")] in owned)

    return source_owned


def _module_owner_matcher(owned: frozenset[str]) -> Callable[[str, object, object], bool]:
    sink_owned = re.compile(r"sink_name=\"?(?:%s)\"?(?:\s|$)" % "|".join(map(re.escape, sorted(owned))) if owned else r"(?!)")

    def module_owned(key: str, _old: object, _new: object) -> bool:
        return sink_owned.search(json.loads(key)["argument"]) is not None

    return module_owned


def compare_audio(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    findings = [
        _scalar_finding("audio", "default_sink", before["default_sink"], after["default_sink"]),
        _scalar_finding("audio", "default_source", before["default_source"], after["default_source"]),
    ]
    findings += AreaComparer("audio.sinks", _owned_by_key(owned), allow_present).compare_items(
        before["sinks"], after["sinks"]
    )
    findings += AreaComparer("audio.sources", _sink_owner_matcher(owned), allow_present).compare_items(
        before["sources"], after["sources"]
    )
    findings += compare_modules(before["modules"], after["modules"], owned, allow_present)
    findings += compare_sink_inputs(before["sink_inputs"], after["sink_inputs"], owned, allow_present)
    return findings


def compare_modules(before: list, after: list, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    def counted(modules: list) -> dict:
        counts = collections.Counter(json.dumps(module, sort_keys=True) for module in modules)
        return {f"{key}#{index}": None for key, count in counts.items() for index in range(count)}

    def module_owned(key: str, old: object, new: object) -> bool:
        return _module_owner_matcher(owned)(key.rsplit("#", 1)[0], old, new)

    findings = AreaComparer("audio.modules", module_owned, allow_present).compare_items(
        counted(before), counted(after)
    )
    for finding in findings:
        finding.key = _module_label(finding.key)
    return findings


def _module_label(key: str) -> str:
    module = json.loads(key.rsplit("#", 1)[0])
    return f"{module['name']} {module['argument'][:60]}".strip()


def compare_sink_inputs(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    findings = []
    comparer = AreaComparer("audio.sink_inputs", _never_owned, allow_present)
    for index in sorted(set(before) | set(after), key=int):
        old, new = before.get(index), after.get(index)
        if old is None or new is None:
            findings.append(_stream_churn(index, old, new))
        elif old == new:
            comparer._add(index, CLASS_RESTORED, "stream unmoved", False, old, new)
        elif {old["sink"], new["sink"]} & owned:
            comparer._add(
                index, CLASS_OWNED_GONE if new["sink"] not in owned else CLASS_OWNED_PRESENT,
                f"stream moved {old['sink']} -> {new['sink']} (owned sink involved)",
                new["sink"] in owned and not allow_present, old, new,
            )
        else:
            comparer._add(index, CLASS_DRIFT, f"stream moved {old['sink']} -> {new['sink']}", True, old, new)
    return findings + comparer.findings


def _stream_churn(index: str, old: dict | None, new: dict | None) -> Finding:
    verb = "ended" if new is None else "started"
    return Finding("audio.sink_inputs", index, CLASS_INFO, f"playback stream {verb} (ordinary desktop churn)", False, old, new)


def compare_processes(before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    def tracked(data: dict) -> dict:
        return {
            f"{item['group']}:{item['pid']}": item
            for item in data["processes"]
            if item["group"] in PROTECTED_PROCESS_GROUPS
        }

    def pid_owned(key: str, _old: object, _new: object) -> bool:
        return key.rsplit(":", 1)[1] in owned

    findings = AreaComparer("processes", pid_owned, allow_present).compare_items(tracked(before), tracked(after))
    helper_counts = [
        sum(1 for item in data["processes"] if item["group"] == "session_helper") for data in (before, after)
    ]
    findings.append(
        Finding(
            "processes", "session_helper", CLASS_INFO,
            f"vibeshine session helpers/brokers churn by design: {helper_counts[0]} -> {helper_counts[1]}",
            False, helper_counts[0], helper_counts[1],
        )
    )
    return findings


SECTION_COMPARERS: dict[str, Callable[[dict, dict, frozenset[str], bool], list[Finding]]] = {
    "displays": compare_displays,
    "audio": compare_audio,
    "devices": compare_devices,
    "processes": compare_processes,
    "drm_connectors": compare_drm_connectors,
    "kernel_modules": compare_kernel_modules,
    "dbus_names": compare_dbus_names,
    "nvidia_encoder": compare_nvidia_encoder,
    "vaapi": compare_vaapi,
    "listeners": compare_listeners,
}


def _compare_section(name: str, before: dict, after: dict, owned: frozenset[str], allow_present: bool) -> list[Finding]:
    old, new = before["sections"][name], after["sections"][name]
    if old["status"] != "ok" or new["status"] != "ok":
        return [_unverified_finding(name, old, new)]
    try:
        return SECTION_COMPARERS[name](old["data"], new["data"], owned, allow_present)
    except (KeyError, TypeError, AttributeError, IndexError, ValueError) as error:
        raise EvidenceError(f"section {name!r} is malformed: {error!r}") from None


def _unverified_finding(name: str, old: dict, new: dict) -> Finding:
    both_missing = old["status"] != "ok" and new["status"] != "ok"
    reasons = f"baseline: {old.get('reason', 'ok')}; now: {new.get('reason', 'ok')}"
    blocking = name in REQUIRED_SECTIONS or not both_missing
    note = "never counts as restored" if blocking else "advisory evidence unavailable on both sides"
    return Finding(name, "*", CLASS_UNVERIFIED, f"{reasons} ({note})", blocking)


def _compare_informational(before: dict, after: dict) -> list[Finding]:
    findings = []
    if before.get("boot_id") != after.get("boot_id"):
        findings.append(
            Finding("host", "boot_id", CLASS_INFO, "the host rebooted between the readings", False,
                    before.get("boot_id"), after.get("boot_id"))
        )
    disk_before, disk_after = (state["sections"]["disk"] for state in (before, after))
    if disk_before["status"] == disk_after["status"] == "ok":
        for path in DISK_PATHS:
            old = disk_before["data"]["free_bytes"].get(path)
            new = disk_after["data"]["free_bytes"].get(path)
            findings.append(Finding("disk", path, CLASS_INFO, f"free bytes {old} -> {new} (not compared)", False, old, new))
    return findings


def compare_states(
    before: dict, after: dict, owned: Iterable[str] = (), allow_owned_present: bool = False
) -> Comparison:
    """Classify every difference between two captured states.

    Raises EvidenceError when either state is malformed, and UsageError when
    ``owned`` names something that may not be owned.
    """
    validate_state(before, "baseline")
    validate_state(after, "current state")
    if before.get("host") != after.get("host"):
        raise EvidenceError(f"baseline is from host {before.get('host')!r}, not {after.get('host')!r}")
    owned_names = validate_owned_names(owned, before)
    findings: list[Finding] = []
    for name in REQUIRED_SECTIONS + ADVISORY_SECTIONS:
        findings += _compare_section(name, before, after, owned_names, allow_owned_present)
    findings += _compare_informational(before, after)
    return Comparison(findings)


# ---------------------------------------------------------------------------
# Waiting for restoration
# ---------------------------------------------------------------------------


def wait_restored(
    baseline: dict,
    owned: Iterable[str],
    deadline_seconds: float,
    capture: Callable[[], dict] = capture_state,
    sleep: Callable[[float], None] = time.sleep,
    monotonic: Callable[[], float] = time.monotonic,
    poll_interval: float = 2.0,
    allow_owned_present: bool = False,
    log: Callable[[str], None] = lambda message: None,
) -> Comparison:
    """Poll until the host matches ``baseline`` or the deadline passes."""
    deadline = monotonic() + max(0.0, deadline_seconds)
    attempt = 0
    while True:
        attempt += 1
        comparison = compare_states(baseline, capture(), owned, allow_owned_present)
        if comparison.clean:
            return comparison
        remaining = deadline - monotonic()
        log(f"attempt {attempt}: not restored (exit {comparison.exit_code}); {max(0.0, remaining):.0f}s left")
        if remaining <= 0:
            return comparison
        sleep(min(poll_interval, remaining))


# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Check:
    name: str
    status: str  # pass | warn | fail | pending | unavailable
    detail: str

    def as_dict(self) -> dict:
        return dataclasses.asdict(self)


def _section_data(state: dict, name: str) -> dict | None:
    reading = state["sections"][name]
    return reading["data"] if reading["status"] == "ok" else None


def evaluate_preflight(state: dict) -> list[Check]:
    validate_state(state)
    checks = [
        _check_display(state),
        _check_audio(state),
        _check_devices(state),
        _check_helper_socket(state),
        _check_kernel_modules(state),
        _check_privilege(state),
        _check_host_processes(state),
        _check_ports(state),
        _check_disk(state),
        _check_native_lock(state),
        _check_gpu_tools(state),
    ]
    for name in ADVISORY_SECTIONS:
        reading = state["sections"][name]
        if reading["status"] != "ok":
            checks.append(Check(f"evidence:{name}", "unavailable", reading["reason"]))
    return checks


def _unavailable_check(name: str, state: dict, section: str) -> Check | None:
    reading = state["sections"][section]
    if reading["status"] == "ok":
        return None
    return Check(name, "unavailable", f"{section}: {reading['reason']}")


def _check_display(state: dict) -> Check:
    missing = _unavailable_check("display", state, "displays")
    if missing:
        return missing
    outputs = state["sections"]["displays"]["data"]["outputs"]
    notes = []
    desktop = outputs.get("Virtual-1")
    if desktop is None or not (desktop["connected"] and desktop["enabled"]):
        notes.append("Virtual-1 (the live desktop) is not connected and enabled")
    for name, output in sorted(outputs.items()):
        notes.append(_describe_output(name, output))
    owned_candidates = _free_virtual_outputs(state)
    notes.append(f"unconnected Virtual-N candidates: {owned_candidates or 'unknown'}")
    status = "warn" if desktop is None or not (desktop["connected"] and desktop["enabled"]) else "pass"
    return Check("display", status, "; ".join(notes))


def _describe_output(name: str, output: dict) -> str:
    state = "enabled" if output["enabled"] else "disabled"
    connected = "connected" if output["connected"] else "disconnected"
    mode = f"{output['mode']}@{output['refresh_hz']}" if output["mode"] else "no mode"
    return f"{name} {connected} {state} {mode} scale {output['scale']} hdr={output['hdr']} wcg={output['wcg']}"


def _free_virtual_outputs(state: dict) -> list[str]:
    data = _section_data(state, "drm_connectors")
    if data is None:
        return []
    free = []
    for key, value in sorted(data["connectors"].items()):
        connector = key.split("-", 1)[-1]
        if connector.startswith("Virtual-") and connector != "Virtual-1" and value["status"] == "disconnected":
            free.append(connector)
    return free


def _check_audio(state: dict) -> Check:
    missing = _unavailable_check("audio", state, "audio")
    if missing:
        return missing
    audio = state["sections"]["audio"]["data"]
    leftovers = sorted(name for name in audio["sinks"] if name.startswith("amaterasu_"))
    detail = f"default sink {audio['default_sink']!r}, {len(audio['sinks'])} sinks, {len(audio['sink_inputs'])} playback streams"
    if leftovers:
        return Check("audio", "fail", f"{detail}; stale Amaterasu sinks present: {leftovers}")
    return Check("audio", "pass", detail)


def _check_devices(state: dict) -> Check:
    missing = _unavailable_check("devices", state, "devices")
    if missing:
        return missing
    entries = state["sections"]["devices"]["data"]["entries"]
    problems = []
    for path in ("/dev/uinput", "/dev/uhid", "/dev/vibeshine-ds5"):
        entry = entries.get(path, {"exists": False})
        if not entry["exists"]:
            problems.append(f"{path} missing")
        elif entry["access"] != "rw":
            problems.append(f"{path} not read/write for this user ({entry['access']})")
    drm = sorted(path for path in entries if re.fullmatch(r"/dev/dri/(?:card|renderD)\d+", path))
    detail = f"DRM nodes {', '.join(drm) or 'none'}"
    if problems:
        return Check("devices", "warn", f"{detail}; {'; '.join(problems)}")
    return Check("devices", "pass", f"{detail}; uinput, uhid and ds5 are read/write for this user")


def _check_helper_socket(state: dict) -> Check:
    data = _section_data(state, "devices")
    if data is None:
        return Check("vkms-helper-socket", "unavailable", "device evidence unavailable")
    entry = data["entries"].get("/run/vibeshine/vkms-control.sock", {"exists": False})
    if not entry["exists"]:
        return Check("vkms-helper-socket", "warn", "helper socket /run/vibeshine/vkms-control.sock is absent")
    if entry["access"] != "rw":
        return Check(
            "vkms-helper-socket", "pending",
            "helper socket is not writable by this user; operator decision E2 (group vibeshine-vkms) is pending",
        )
    return Check("vkms-helper-socket", "pass", "helper socket is writable by this user")


def _check_kernel_modules(state: dict) -> Check:
    data = _section_data(state, "kernel_modules")
    if data is None:
        return Check("kernel-modules", "unavailable", state["sections"]["kernel_modules"]["reason"])
    absent = sorted(name for name, module in data["modules"].items() if not module["loaded"])
    return Check("kernel-modules", "warn" if absent else "pass", f"not loaded: {absent}" if absent else "all loaded")


def _check_privilege(state: dict) -> Check:
    identity = _section_data(state, "identity")
    if identity is None:
        return Check("privilege", "unavailable", state["sections"]["identity"]["reason"])
    if identity["cap_sys_admin"]:
        return Check("privilege", "pass", "CAP_SYS_ADMIN is effective")
    return Check(
        "privilege", "pending",
        "CAP_SYS_ADMIN is not effective; vibeshine_drm frame ioctls need operator decision E3 (workers never run setcap or sudo)",
    )


def _check_host_processes(state: dict) -> Check:
    data = _section_data(state, "processes")
    if data is None:
        return Check("vibeshine-host", "unavailable", state["sections"]["processes"]["reason"])
    groups = collections.Counter(item["group"] for item in data["processes"])
    hosts = sorted(item["pid"] for item in data["processes"] if item["group"] == "host")
    detail = f"host pids {hosts}; steam={groups['steam']} game={groups['game']} session_helper={groups['session_helper']}"
    return Check("vibeshine-host", "pass" if hosts else "warn", detail if hosts else f"no running Vibeshine host; {detail}")


def _check_ports(state: dict) -> Check:
    data = _section_data(state, "listeners")
    if data is None:
        return Check("candidate-ports", "unavailable", state["sections"]["listeners"]["reason"])
    busy = sorted({f"{item['proto']}/{item['port']}" for item in data["listeners"]})
    detail = "qualification must use loopback ephemeral ports; busy default ports (the live host): " + (", ".join(busy) or "none")
    return Check("candidate-ports", "pass", detail)


def _check_disk(state: dict) -> Check:
    data = _section_data(state, "disk")
    if data is None:
        return Check("disk", "unavailable", state["sections"]["disk"]["reason"])
    free = data["free_bytes"]
    gib = {path: free[path] / 1024**3 for path in free}
    detail = ", ".join(f"{path} {value:.1f} GiB free" for path, value in gib.items())
    if free["/home"] < MIN_HOME_FREE_BYTES or free["/tmp"] < MIN_TMP_FREE_BYTES:
        return Check("disk", "fail", f"{detail}; stop and tell the integrator below 12 GiB on /home or 1 GiB on /tmp")
    return Check("disk", "pass", detail)


def _check_native_lock(state: dict) -> Check:
    data = _section_data(state, "native_lock")
    if data is None:
        return Check("native-lock", "unavailable", state["sections"]["native_lock"]["reason"])
    state_name = data["state"]
    if state_name == "held_by_other":
        return Check("native-lock", "warn", f"{data['path']} is held by pid {data['holder_pid']}; wait, do not bypass")
    if state_name == "absent":
        return Check("native-lock", "warn", f"{data['path']} does not exist yet (flock creates it); the integrator owns its creation")
    return Check("native-lock", "pass", f"{data['path']} is {state_name.replace('_', ' ')}")


def _check_gpu_tools(state: dict) -> Check:
    nvidia = _section_data(state, "nvidia_encoder")
    vaapi = _section_data(state, "vaapi")
    parts = []
    if nvidia is None:
        parts.append("nvidia-smi: " + state["sections"]["nvidia_encoder"]["reason"])
    else:
        parts.append("nvidia encoder sessions: " + ", ".join(f"{gpu['name']}={gpu['encoder_sessions']}" for gpu in nvidia["gpus"]))
    parts.append("vaapi: " + ("present" if vaapi else state["sections"]["vaapi"]["reason"]))
    return Check("gpu-tools", "pass" if nvidia else "warn", "; ".join(parts))


def preflight_exit_code(state: dict, checks: list[Check]) -> int:
    if unavailable_required_sections(state):
        return 2
    return 1 if any(check.status == "fail" for check in checks) else 0


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------

STATUS_ORDER = {"fail": 0, "unavailable": 1, "pending": 2, "warn": 3, "pass": 4}


def format_preflight(checks: list[Check]) -> str:
    lines = ["Linux session preflight (read-only host evidence; no milestone is claimed)"]
    for check in sorted(checks, key=lambda item: STATUS_ORDER[item.status]):
        lines.append(f"  [{check.status:^11}] {check.name}: {check.detail}")
    return "\n".join(lines)


def format_comparison(comparison: Comparison, verbose: bool = False) -> str:
    lines = []
    counts = collections.Counter(f.classification for f in comparison.findings)
    for finding in comparison.findings:
        if finding.classification == CLASS_RESTORED and not verbose:
            continue
        lines.append(f"  [{finding.classification}] {finding.area} {finding.key}: {finding.detail}")
    summary = ", ".join(f"{counts[name]} {name}" for name in sorted(counts))
    verdict = {0: "CLEAN", 1: "NOT RESTORED (drift)", 2: "EVIDENCE UNAVAILABLE"}[comparison.exit_code]
    return "\n".join(lines + [f"{verdict}: {summary}"])


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Read-only Linux host snapshot and restoration comparator.")
    commands = parser.add_subparsers(dest="command", required=True)
    preflight = commands.add_parser("preflight", help="report readiness without changing anything")
    preflight.add_argument("--json", action="store_true", help="print machine-readable JSON")
    snapshot = commands.add_parser("snapshot", help="write a private baseline JSON")
    snapshot.add_argument("--out", required=True, help="new file inside a private (0700) directory")
    snapshot.add_argument("--port", type=int, action="append", default=[], help="extra candidate test port")
    compare = commands.add_parser("compare", help="compare the host with a baseline")
    compare.add_argument("--baseline", required=True)
    compare.add_argument("--owned", nargs="+", action="extend", default=[], metavar="NAME")
    compare.add_argument("--wait-restored", type=float, metavar="SECONDS", default=None)
    compare.add_argument("--poll-interval", type=float, default=2.0)
    compare.add_argument("--allow-owned-present", action="store_true",
                         help="do not fail while a named owned resource still exists")
    compare.add_argument("--json", action="store_true")
    compare.add_argument("-v", "--verbose", action="store_true", help="also list restored items")
    return parser


def command_preflight(arguments: argparse.Namespace, capture: Callable[[], dict]) -> int:
    state = capture()
    checks = evaluate_preflight(state)
    code = preflight_exit_code(state, checks)
    if arguments.json:
        print(json.dumps({"exit_code": code, "checks": [c.as_dict() for c in checks], "state": state}, indent=2, sort_keys=True))
    else:
        print(format_preflight(checks))
    return code


def command_snapshot(arguments: argparse.Namespace, capture: Callable[[], dict]) -> int:
    state = capture()
    missing = unavailable_required_sections(state)
    if missing:
        for name, reason in missing:
            print(f"error: required evidence {name} unavailable: {reason}", file=sys.stderr)
        print("error: refusing to write an incomplete baseline", file=sys.stderr)
        return 2
    write_private_json(arguments.out, state)
    advisory = [name for name in ADVISORY_SECTIONS if state["sections"][name]["status"] != "ok"]
    print(f"baseline written to {arguments.out} at {state['captured_at']} on {state['host']}")
    if advisory:
        print(f"advisory evidence unavailable (recorded with reasons): {', '.join(advisory)}")
    return 0


def command_compare(arguments: argparse.Namespace, capture: Callable[[], dict]) -> int:
    baseline = load_state_file(arguments.baseline)
    validate_owned_names(arguments.owned, baseline)
    deadline = arguments.wait_restored if arguments.wait_restored is not None else 0.0
    comparison = wait_restored(
        baseline, arguments.owned, deadline, capture=capture,
        poll_interval=arguments.poll_interval,
        allow_owned_present=arguments.allow_owned_present,
        log=lambda message: print(message, file=sys.stderr),
    )
    if arguments.json:
        report = {"exit_code": comparison.exit_code, "findings": [f.as_dict() for f in comparison.findings]}
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(format_comparison(comparison, arguments.verbose))
    return comparison.exit_code


def main(argv: Sequence[str] | None = None, capture: Callable[[], dict] | None = None) -> int:
    parser = build_parser()
    try:
        arguments = parser.parse_args(argv)
    except SystemExit as exit_request:
        return 2 if exit_request.code else 0
    if capture is None:
        extra_ports = getattr(arguments, "port", [])
        capture = lambda: capture_state(extra_ports=extra_ports)  # noqa: E731
    handlers = {"preflight": command_preflight, "snapshot": command_snapshot, "compare": command_compare}
    try:
        return handlers[arguments.command](arguments, capture)
    except (EvidenceError, UsageError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
