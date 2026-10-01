#!/usr/bin/env python3
"""Unit tests for scripts/linux_host_snapshot.py (read-only host baseline tool).

Run with: python3 scripts/test_linux_session_preflight.py

The tests inject command output instead of touching the host.  The kscreen-doctor
and pactl fixtures below were captured read-only from the live KDE host on
2026-09-30 and trimmed (mode lists, volumes, ports, properties).  The one
playback-stream (sink-input) record is synthetic, modelled on the pactl JSON
schema, because the host had no playback streams when it was captured.  These
tests establish parsing and comparison behaviour only; they say nothing about a
streaming milestone, and they never run a native or mutating command.
"""

from __future__ import annotations

import ast
import contextlib
import copy
import importlib.util
import io
import json
import os
import re
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

SCRIPT = Path(__file__).resolve().parent / "linux_host_snapshot.py"
_spec = importlib.util.spec_from_file_location("linux_host_snapshot", SCRIPT)
snapshot = importlib.util.module_from_spec(_spec)
sys.modules["linux_host_snapshot"] = snapshot
_spec.loader.exec_module(snapshot)

# kscreen-doctor -j captured read-only on this host 2026-09-30; mode lists trimmed to the current, preferred and a few neighbouring modes.
KSCREEN_JSON = r'''{
 "features": 255,
 "outputs": [
  {
   "brightness": 1,
   "clones": [],
   "connected": true,
   "currentModeId": "1",
   "enabled": true,
   "followPreferredMode": false,
   "hdr": true,
   "iccProfilePath": "",
   "icon": "",
   "id": 1,
   "maxBpc": 0,
   "modes": [
    {
     "id": "1",
     "name": "3840x2160@120",
     "refreshRate": 120,
     "size": {
      "height": 2160,
      "width": 3840
     }
    },
    {
     "id": "2",
     "name": "4096x2160@60",
     "refreshRate": 60,
     "size": {
      "height": 2160,
      "width": 4096
     }
    },
    {
     "id": "50",
     "name": "3024x1890@120",
     "refreshRate": 119.52300262451172,
     "size": {
      "height": 1890,
      "width": 3024
     }
    },
    {
     "id": "55",
     "name": "3024x1890@120",
     "refreshRate": 119.91500091552734,
     "size": {
      "height": 1890,
      "width": 3024
     }
    }
   ],
   "name": "Virtual-1",
   "pos": {
    "x": 0,
    "y": 0
   },
   "preferredModes": [
    "1"
   ],
   "priority": 1,
   "replicationSource": 0,
   "rotation": 1,
   "scale": 2.5,
   "sdr-brightness": 330,
   "size": {
    "height": 2160,
    "width": 3840
   },
   "sizeMM": {
    "height": 340,
    "width": 600
   },
   "type": 0,
   "vrrPolicy": 1,
   "wcg": true
  },
  {
   "brightness": 1,
   "clones": [],
   "connected": true,
   "currentModeId": "58",
   "enabled": false,
   "followPreferredMode": false,
   "hdr": false,
   "iccProfilePath": "",
   "icon": "",
   "id": 2,
   "modes": [
    {
     "id": "57",
     "name": "3840x2160@60",
     "refreshRate": 60,
     "size": {
      "height": 2160,
      "width": 3840
     }
    },
    {
     "id": "58",
     "name": "3840x2160@120",
     "refreshRate": 119.87999725341797,
     "size": {
      "height": 2160,
      "width": 3840
     }
    },
    {
     "id": "59",
     "name": "3840x2160@100",
     "refreshRate": 100,
     "size": {
      "height": 2160,
      "width": 3840
     }
    }
   ],
   "name": "HDMI-A-1",
   "pos": {
    "x": 0,
    "y": 0
   },
   "preferredModes": [
    "57"
   ],
   "priority": 2,
   "replicationSource": 0,
   "rotation": 1,
   "scale": 2,
   "sdr-brightness": 200,
   "size": {
    "height": 2160,
    "width": 3840
   },
   "sizeMM": {
    "height": 900,
    "width": 1600
   },
   "type": 6,
   "vrrPolicy": 2,
   "wcg": false
  }
 ],
 "screen": {
  "currentSize": {
   "height": 864,
   "width": 1536
  },
  "id": 0,
  "maxActiveOutputsCount": 2,
  "maxSize": {
   "height": 64000,
   "width": 64000
  },
  "minSize": {
   "height": 0,
   "width": 0
  }
 },
 "tabletModeAvailable": true,
 "tabletModeEngaged": false
}'''

# kscreen-doctor -o captured read-only (ANSI colour escapes stripped; the parser strips them too); mode lists trimmed.
KSCREEN_TEXT = r'''Output: 1 Virtual-1 c567e2f2-c85a-4c90-904c-5bfce8a80703
	enabled
	connected
	priority 1
	Unknown
	replication source:0
	Modes: 1:3840x2160@120.00*!  2:4096x2160@60.00  50:3024x1890@119.52  55:3024x1890@119.92
	Custom modes:
		0: 3024x1890@119.52 (reduced blanking)
		1: 3840x2160@119.72 (reduced blanking)
		2: 2560x1440@115.60 (reduced blanking)
		3: 3840x2160@115.88 (reduced blanking)
		4: 2560x1440@115.72 (reduced blanking)
		5: 3024x1890@119.92 (reduced blanking)
		6: 2560x1440@115.89 (reduced blanking)
	Geometry: 0,0 1536x864
	Scale: 2.5
	Rotation: 1
	Overscan: 0
	Vrr: Always
	RgbRange: unknown
	HDR: enabled
		SDR brightness: 330 nits
		SDR gamut wideness: 0%
		Peak brightness: 993 nits, overridden with: 1230 nits
		Max average brightness: 591 nits
		Min brightness: 0.0002 nits
		HDR color profile source: EDID
		HDR ICC profile: none
	Wide Color Gamut: enabled
	ICC profile: none
	Color profile source: sRGB
	Color power preference: prefer accuracy
	Brightness control: supported, set to 100% and dimming to 100%
	Color resolution: automatic (16), range: [8; 16] bits per color
	Allow EDR: unsupported
	Sharpness control: unsupported
	Automatic brightness: unsupported
	Auto Rotate Policy: incapable
	Adaptive backlight modulation: unsupported
Output: 2 HDMI-A-1 df620ee4-11bd-4aae-9ec2-b8a38560b522
	disabled
	connected
	priority 2
	HDMI
	replication source:0
	Modes: 57:3840x2160@60.00!  58:3840x2160@119.88*  59:3840x2160@100.00
	Custom modes: None
	Geometry: 0,0 1920x1080
	Scale: 2
	Rotation: 1
	Overscan: 0
	Vrr: Automatic
	RgbRange: unknown
	HDR: disabled
	Wide Color Gamut: disabled
	ICC profile: none
	Color profile source: sRGB
	Color power preference: prefer efficiency and performance
	Brightness control: supported, set to 100% and dimming to 100%
	Color resolution: unknown
	Allow EDR: unsupported
	Sharpness control: unsupported
	Automatic brightness: unsupported
	Auto Rotate Policy: incapable
	Adaptive backlight modulation: unsupported
'''

# pactl info captured read-only (cookie line removed).
PACTL_INFO = r'''Server String: /run/user/1000/pulse/native
Library Protocol Version: 35
Server Protocol Version: 35
Is Local: yes
Client Index: 2941
Tile Size: 65472
User Name: chasep
Host Name: AmbidexL
Server Name: PulseAudio (on PipeWire 1.6.9)
Server Version: 15.0.0
Default Sample Specification: float32le 2ch 48000Hz
Default Channel Map: front-left,front-right
Default Sink: sink-sunshine-stereo
Default Source: alsa_input.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.mono-fallback
'''

# pactl -f json list sinks captured read-only; volume, ports, formats and properties trimmed; three of six sinks kept.
PACTL_SINKS = r'''[
 {
  "index": 49,
  "state": "SUSPENDED",
  "name": "alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo",
  "description": "Arctis Nova Pro Wireless Analog Stereo",
  "driver": "PipeWire",
  "sample_specification": "s24le 2ch 48000Hz",
  "channel_map": "front-left,front-right",
  "owner_module": 4294967295,
  "mute": false,
  "monitor_source": "alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo.monitor",
  "flags": [
   "HARDWARE",
   "HW_MUTE_CTRL",
   "DECIBEL_VOLUME",
   "LATENCY"
  ],
  "active_port": "analog-output"
 },
 {
  "index": 54,
  "state": "SUSPENDED",
  "name": "alsa_output.pci-0000_00_1f.3.iec958-stereo",
  "description": "Built-in Audio Digital Stereo (IEC958)",
  "driver": "PipeWire",
  "sample_specification": "s32le 2ch 48000Hz",
  "channel_map": "front-left,front-right",
  "owner_module": 4294967295,
  "mute": false,
  "monitor_source": "alsa_output.pci-0000_00_1f.3.iec958-stereo.monitor",
  "flags": [
   "HARDWARE",
   "HW_MUTE_CTRL",
   "DECIBEL_VOLUME",
   "LATENCY",
   "SET_FORMATS"
  ],
  "active_port": "iec958-stereo-output"
 },
 {
  "index": 2801,
  "state": "IDLE",
  "name": "sink-sunshine-stereo",
  "description": "sink-sunshine-stereo",
  "driver": "PipeWire",
  "sample_specification": "float32le 2ch 48000Hz",
  "channel_map": "front-left,front-right",
  "owner_module": 536870918,
  "mute": false,
  "monitor_source": "sink-sunshine-stereo.monitor",
  "flags": [
   "HARDWARE",
   "DECIBEL_VOLUME",
   "LATENCY"
  ],
  "active_port": null
 }
]'''

# pactl -f json list sources captured read-only, trimmed likewise.
PACTL_SOURCES = r'''[
 {
  "index": 49,
  "state": "SUSPENDED",
  "name": "alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo.monitor",
  "description": "Monitor of Arctis Nova Pro Wireless Analog Stereo",
  "driver": "PipeWire",
  "sample_specification": "s24le 2ch 48000Hz",
  "channel_map": "front-left,front-right",
  "owner_module": 4294967295,
  "mute": false,
  "monitor_source": "alsa_output.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.analog-stereo",
  "flags": [
   "HARDWARE",
   "DECIBEL_VOLUME",
   "LATENCY"
  ],
  "active_port": "analog-output"
 },
 {
  "index": 50,
  "state": "SUSPENDED",
  "name": "alsa_input.usb-SteelSeries_Arctis_Nova_Pro_Wireless-00.mono-fallback",
  "description": "Arctis Nova Pro Wireless Mono",
  "driver": "PipeWire",
  "sample_specification": "s16le 1ch 48000Hz",
  "channel_map": "mono",
  "owner_module": 4294967295,
  "mute": false,
  "monitor_source": "",
  "flags": [
   "HARDWARE",
   "HW_MUTE_CTRL",
   "HW_VOLUME_CTRL",
   "DECIBEL_VOLUME",
   "LATENCY"
  ],
  "active_port": "analog-input-mic"
 },
 {
  "index": 2801,
  "state": "IDLE",
  "name": "sink-sunshine-stereo.monitor",
  "description": "Monitor of sink-sunshine-stereo",
  "driver": "PipeWire",
  "sample_specification": "float32le 2ch 48000Hz",
  "channel_map": "front-left,front-right",
  "owner_module": 536870918,
  "mute": false,
  "monitor_source": "sink-sunshine-stereo",
  "flags": [
   "HARDWARE",
   "DECIBEL_VOLUME",
   "LATENCY"
  ],
  "active_port": null
 }
]'''

# pactl -f json list modules captured read-only; properties trimmed and three of twenty modules kept (pipewire-pulse reports no module index).
PACTL_MODULES = r'''[
 {
  "name": "libpipewire-module-profiler",
  "argument": "{\n            #profile.interval.ms = 0\n        }",
  "usage_counter": "n/a"
 },
 {
  "name": "module-always-sink",
  "argument": "",
  "usage_counter": "n/a"
 },
 {
  "name": "module-null-sink",
  "argument": "sink_name=sink-sunshine-stereo format=float32 rate=48000 channels=2 channel_map=front-left,front-right sink_properties=device.description=sink-sunshine-stereo",
  "usage_counter": "n/a"
 }
]'''

SYNTHETIC_SINK_INPUTS = json.dumps(
    [
        {
            "index": 4001,
            "driver": "PipeWire",
            "owner_module": 4294967295,
            "client": "4000",
            "sink": 2801,
            "sample_specification": "float32le 2ch 48000Hz",
            "channel_map": "front-left,front-right",
            "properties": {
                "application.name": "mpv",
                "application.process.binary": "mpv",
                "application.process.id": "31337",
                "media.name": "fixture tone",
            },
        }
    ]
)

STAT_RECORDS = {
    "/dev/dri/by-path": ("directory", "755", "root", "root", "0:0"),
    "/dev/dri/card0": ("character special file", "660", "root", "video", "e2:0"),
    "/dev/dri/card1": ("character special file", "660", "root", "video", "e2:1"),
    "/dev/dri/card2": ("character special file", "660", "root", "video", "e2:2"),
    "/dev/dri/renderD128": ("character special file", "666", "root", "render", "e2:80"),
    "/dev/uhid": ("character special file", "660", "root", "vibeshine-uinput", "a:ef"),
    "/dev/uinput": ("character special file", "660", "root", "vibeshine-uinput", "a:df"),
    "/dev/vibeshine-ds5": ("character special file", "660", "root", "vibeshine-uinput", "a:106"),
    "/run/vibeshine/vkms-control.sock": ("socket", "660", "root", "vibeshine-vkms", "0:0"),
}
USER_ACL = "user::rw-\nuser:chasep:rw-\ngroup::rw-\nmask::rw-\nother::---\n"
ACLS = {
    "/dev/dri/by-path": "user::rwx\ngroup::r-x\nother::r-x\n",
    "/dev/dri/card0": USER_ACL,
    "/dev/dri/card1": USER_ACL,
    "/dev/dri/card2": USER_ACL,
    "/dev/dri/renderD128": "user::rw-\ngroup::rw-\nother::rw-\n",
    "/dev/uhid": USER_ACL,
    "/dev/uinput": USER_ACL,
    "/dev/vibeshine-ds5": USER_ACL,
    "/run/vibeshine/vkms-control.sock": "user::rw-\ngroup::rw-\nother::---\n",
}
ACCESS = {
    "/dev/uhid": "rw", "/dev/uinput": "rw", "/dev/vibeshine-ds5": "rw",
    "/run/vibeshine/vkms-control.sock": "--",
}
PROC_MODULES = (
    "uinput 28672 5 - Live 0x0000000000000000\n"
    "vibeshine_drm 135168 38 - Live 0x0000000000000000 (OE)\n"
    "vibeshine_ds5 110592 1 - Live 0x0000000000000000 (OE)\n"
    "uhid 24576 0 - Live 0x0000000000000000\n"
)
# Real command lines from this host with arguments abbreviated.
PROCESS_FIXTURES = (
    (1838, "vibeshine-sessi", 1, "35", ["/usr/bin/bash", "/usr/libexec/vibeshine/vibeshine-session-controller", "run"]),
    (5202, "vibeshine-machi", 1, "40", ["/usr/bin/bash", "/usr/libexec/vibeshine/vibeshine-machine-host", "run"]),
    (5237, "vibeshine-host", 5202, "41", ["/usr/libexec/vibeshine/vibeshine-host", "/var/lib/vibeshine/vibeshine.conf", "capture=kms"]),
    (5439, "steam", 4109, "50", ["/home/chasep/.local/share/Steam/ubuntu12_32/steam", "-srt-logger-opened"]),
    (148583, "wineserver", 1, "900", ["/usr/lib/wine/../../bin/wineserver"]),
    (273202, "vibeshine-sessi", 1, "9000", ["/usr/libexec/vibeshine/vibeshine-session-broker"]),
    (4238, "srt-logger", 3062, "55", ["/home/chasep/.local/share/Steam/ubuntu12_32/steam-runtime/usr/libexec/steam-runtime-tools-0/srt-logger", "--sh-syntax"]),
)
NET_TCP = (
    "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n"
    "   0: 00000000:BB75 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 11 1 0 100 0 0 10 0\n"
    "   1: 0100007F:9239 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12 1 0 100 0 0 10 0\n"
    "   2: 0100007F:BB75 0100007F:D5A0 01 00000000:00000000 00:00000000 00000000     0        0 13 1 0 100 0 0 10 0\n"
)
NET_UDP = (
    "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n"
    "  100: 00000000:BB7E 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 21 2 0 0\n"
)
DRM_STATUS = {
    "card0-DP-4": "disconnected", "card1-HDMI-A-1": "connected",
    "card2-Virtual-1": "connected", "card2-Virtual-2": "disconnected",
    "card2-Virtual-3": "disconnected", "card2-Virtual-4": "disconnected",
}
NVIDIA_QUERY = "0, GPU-fixture-uuid, NVIDIA GeForce RTX 4090, 0\n"


def proc_stat_line(pid: int, comm: str, ppid: int, starttime: str) -> str:
    return f"{pid} ({comm}) S {ppid} " + "0 " * 17 + f"{starttime} 0 0\n"


class FixtureProbe(snapshot.HostProbe):
    """A host described entirely by the fixtures above."""

    proc_root = "/fake/proc"
    drm_sysfs_root = "/fake/sys/class/drm"
    native_lock_path = "/nonexistent-amaterasu-fixture/native.lock"

    def __init__(self, kscreen_json: str = KSCREEN_JSON) -> None:
        self.argv_log: list[list[str]] = []
        self.missing_tools: set[str] = set()
        self.kscreen_json = kscreen_json
        self.kscreen_text = KSCREEN_TEXT
        self.sink_inputs = "[]"
        self.sinks = PACTL_SINKS
        self.sources = PACTL_SOURCES
        self.modules = PACTL_MODULES
        self.pactl_info = PACTL_INFO
        self.stat_records = dict(STAT_RECORDS)
        self.acls = dict(ACLS)
        self.nvidia_query = NVIDIA_QUERY
        self.dbus = {name: True for name in snapshot.DBUS_NAMES}
        self.free_bytes = {"/home": 38 * 1024**3, "/tmp": 24 * 1024**3}
        self.processes = list(PROCESS_FIXTURES)
        self.files = {
            "/fake/proc/modules": PROC_MODULES,
            "/fake/proc/net/tcp": NET_TCP,
            "/fake/proc/net/udp": NET_UDP,
            "/fake/proc/sys/kernel/random/boot_id": "11111111-2222-3333-4444-555555555555\n",
            "/fake/proc/self/status": "Name:\tpython3\nCapEff:\t0000000000000000\n",
            "/fake/sys/class/drm/version": "drm 1.1.0\n",
        }
        for name, status in DRM_STATUS.items():
            self.files[f"/fake/sys/class/drm/{name}/status"] = status + "\n"
        self.install_processes()

    def install_processes(self) -> None:
        for key in [key for key in self.files if key.startswith("/fake/proc/") and key.split("/")[3].isdigit()]:
            del self.files[key]
        for pid, comm, ppid, start, argv in self.processes:
            self.files[f"/fake/proc/{pid}/stat"] = proc_stat_line(pid, comm, ppid, start)
            self.files[f"/fake/proc/{pid}/cmdline"] = "\0".join(argv) + "\0"

    def run(self, argv):
        snapshot.validate_argv(argv)  # the real runner does this too
        self.argv_log.append(list(argv))
        tool = os.path.basename(argv[0])
        if tool in self.missing_tools:
            raise snapshot.ToolUnavailable(f"{tool} not found")
        dispatch = {
            "kscreen-doctor": self._kscreen, "pactl": self._pactl, "stat": self._stat,
            "getfacl": self._getfacl, "nvidia-smi": lambda _a: snapshot.CommandResult(0, self.nvidia_query),
        }
        return dispatch[tool](argv[1:])

    def _kscreen(self, arguments):
        return snapshot.CommandResult(0, self.kscreen_json if arguments == ["-j"] else self.kscreen_text)

    def _pactl(self, arguments):
        if arguments == ["info"]:
            return snapshot.CommandResult(0, self.pactl_info)
        kind = arguments[-1]
        texts = {"sinks": self.sinks, "sources": self.sources, "modules": self.modules, "sink-inputs": self.sink_inputs}
        return snapshot.CommandResult(0, texts[kind])

    def _stat(self, arguments):
        path = arguments[1]
        return snapshot.CommandResult(0, "\t".join(self.stat_records[path]) + "\n")

    def _getfacl(self, arguments):
        return snapshot.CommandResult(0, self.acls[arguments[1]])

    def which(self, tool):
        return None if tool in self.missing_tools or tool == "vainfo" else f"/usr/bin/{tool}"

    def glob(self, pattern):
        assert pattern == "/dev/dri/*", pattern
        return sorted(path for path in self.stat_records if path.startswith("/dev/dri/"))

    def lexists(self, path):
        return path in self.stat_records

    def read_text(self, path):
        if path not in self.files:
            raise FileNotFoundError(path)
        return self.files[path]

    def list_dir(self, path):
        prefix = path.rstrip("/") + "/"
        names = {key[len(prefix):].split("/")[0] for key in self.files if key.startswith(prefix)}
        if not names:
            raise FileNotFoundError(path)
        return sorted(names)

    def access_string(self, path):
        return ACCESS.get(path, "--")

    def disk_free_bytes(self, path):
        return self.free_bytes[path]

    def process_owner(self, pid):
        return "vibeshine" if int(pid) in (5237, 5202) else "chasep"

    def dbus_names_with_owner(self, names):
        return {name: self.dbus[name] for name in names}

    def hostname(self):
        return "fixture-host"

    def now(self):
        return 1_790_000_000.0

    def own_pid(self):
        return 424242

    def identity(self):
        return {"uid": 1000, "user": "chasep", "groups": ["audio", "video"], "cap_sys_admin": False}


def capture(probe: FixtureProbe | None = None) -> dict:
    return snapshot.capture_state(probe or FixtureProbe())


def edit(state: dict, mutate) -> dict:
    changed = copy.deepcopy(state)
    mutate(changed)
    return changed


def section(state: dict, name: str) -> dict:
    return state["sections"][name]["data"]


def findings_for(comparison, area: str, key: str | None = None) -> list:
    return [f for f in comparison.findings if f.area == area and (key is None or f.key == key)]


def with_virtual_2(state: dict) -> dict:
    """The state while an owned Virtual-2 exists (KScreen and DRM sysfs agree)."""
    def mutate(changed):
        outputs = section(changed, "displays")["outputs"]
        outputs["Virtual-2"] = dict(outputs["Virtual-1"], position=[3840, 0], priority=3, hdr=False, wcg=False, scale=1.0)
        section(changed, "drm_connectors")["connectors"]["card2-Virtual-2"] = {"status": "connected"}
    return edit(state, mutate)


def with_owned_sink(state: dict, name: str = "amaterasu_qual_42", move_input: bool = True) -> dict:
    def mutate(changed):
        audio = section(changed, "audio")
        audio["sinks"][name] = {
            "description": "AmaterasuAudio", "driver": "PipeWire", "sample_spec": "float32le 2ch 48000Hz",
            "channel_map": "front-left,front-right", "monitor_source": f"{name}.monitor", "module_owned": True,
        }
        audio["sources"][f"{name}.monitor"] = {
            "description": "Monitor of AmaterasuAudio", "driver": "PipeWire",
            "sample_spec": "float32le 2ch 48000Hz", "channel_map": "front-left,front-right", "module_owned": True,
        }
        audio["modules"].append({"name": "module-null-sink", "argument": f"sink_name={name} rate=48000"})
        if move_input:
            for stream in audio["sink_inputs"].values():
                stream["sink"] = name
    return edit(state, mutate)


class ParserTests(unittest.TestCase):
    def test_kscreen_json_parses_virtual_1_and_hdmi_with_hdr_and_scale(self):
        outputs = snapshot.parse_kscreen_json(KSCREEN_JSON)
        virtual = outputs["Virtual-1"]
        self.assertEqual(
            (virtual["connected"], virtual["enabled"], virtual["hdr"], virtual["wcg"], virtual["scale"]),
            (True, True, True, True, 2.5),
        )
        self.assertEqual((virtual["mode"], virtual["refresh_hz"], virtual["position"]), ("3840x2160", 120.0, [0, 0]))
        self.assertEqual((virtual["priority"], virtual["rotation"]), (1, 1))
        hdmi = outputs["HDMI-A-1"]
        self.assertEqual(
            (hdmi["connected"], hdmi["enabled"], hdmi["hdr"], hdmi["wcg"], hdmi["scale"]),
            (True, False, False, False, 2.0),
        )
        self.assertEqual((hdmi["mode"], hdmi["refresh_hz"]), ("3840x2160", 119.88))
        self.assertEqual(sorted(outputs), ["HDMI-A-1", "Virtual-1"])

    def test_kscreen_text_fallback_agrees_with_json(self):
        self.assertEqual(snapshot.parse_kscreen_text(KSCREEN_TEXT), snapshot.parse_kscreen_json(KSCREEN_JSON))

    def test_kscreen_text_strips_ansi_colour_codes(self):
        coloured = KSCREEN_TEXT.replace("enabled\n", "\x1b[01;32menabled\x1b[0;0m\n", 1)
        self.assertEqual(snapshot.parse_kscreen_text(coloured), snapshot.parse_kscreen_text(KSCREEN_TEXT))

    def test_capture_falls_back_to_text_when_json_fails(self):
        probe = FixtureProbe(kscreen_json="")
        data = section(capture(probe), "displays")
        self.assertEqual(data["source"], "kscreen-doctor -o")
        self.assertTrue(data["outputs"]["Virtual-1"]["hdr"])

    def test_pactl_info_and_lists(self):
        state = capture()
        audio = section(state, "audio")
        self.assertEqual(audio["default_sink"], "sink-sunshine-stereo")
        self.assertIn("Arctis_Nova_Pro_Wireless-00.mono-fallback", audio["default_source"])
        self.assertEqual(len(audio["sinks"]), 3)
        self.assertTrue(audio["sinks"]["sink-sunshine-stereo"]["module_owned"])
        self.assertFalse(audio["sinks"]["alsa_output.pci-0000_00_1f.3.iec958-stereo"]["module_owned"])
        names = [module["name"] for module in audio["modules"]]
        self.assertIn("module-null-sink", names)
        self.assertNotIn("index", audio["modules"][0])

    def test_sink_input_resolves_its_sink_by_name(self):
        probe = FixtureProbe()
        probe.sink_inputs = SYNTHETIC_SINK_INPUTS
        streams = section(capture(probe), "audio")["sink_inputs"]
        self.assertEqual(streams["4001"]["sink"], "sink-sunshine-stereo")
        self.assertEqual(streams["4001"]["binary"], "mpv")

    def test_devices_record_type_mode_owner_acl_and_access(self):
        entries = section(capture(), "devices")["entries"]
        uinput = entries["/dev/uinput"]
        self.assertEqual((uinput["type"], uinput["mode"], uinput["group"]), ("character special file", "660", "vibeshine-uinput"))
        self.assertIn("user:chasep:rw-", uinput["acl"])
        self.assertEqual(uinput["access"], "rw")
        self.assertEqual(entries["/run/vibeshine/vkms-control.sock"]["access"], "--")
        self.assertTrue(entries["/dev/vibeshine-ds5"]["exists"])

    def test_absent_device_is_recorded_as_absent(self):
        probe = FixtureProbe()
        del probe.stat_records["/dev/uhid"]
        self.assertEqual(section(capture(probe), "devices")["entries"]["/dev/uhid"], {"exists": False})

    def test_kernel_modules_ignore_reference_counts(self):
        modules = section(capture(), "kernel_modules")["modules"]
        self.assertEqual(modules["vibeshine_drm"], {"loaded": True, "state": "Live"})

    def test_proc_net_addresses_decode(self):
        self.assertEqual(snapshot.decode_proc_net_address("0100007F:BB75"), ("127.0.0.1", 47989))
        self.assertEqual(snapshot.decode_proc_net_address("00000000000000000000000001000000:1F90"), ("::1", 8080))
        listeners = section(capture(), "listeners")["listeners"]
        self.assertIn({"proto": "tcp", "address": "0.0.0.0", "port": 47989}, listeners)
        self.assertIn({"proto": "udp", "address": "0.0.0.0", "port": 47998}, listeners)
        self.assertFalse([item for item in listeners if item["port"] == 37433], "non-candidate port recorded")
        self.assertEqual(len([i for i in listeners if i["proto"] == "tcp"]), 1, "an established connection is not a listener")

    def test_nvidia_and_vaapi_readings(self):
        state = capture()
        self.assertEqual(section(state, "nvidia_encoder")["gpus"][0]["encoder_sessions"], 0)
        self.assertEqual(state["sections"]["vaapi"]["status"], "unavailable")
        profiles = snapshot.parse_vainfo(
            "vainfo: Driver version: Mesa Gallium driver\n"
            "      VAProfileH264Main               : VAEntrypointVLD\n"
            "      VAProfileH264Main               : VAEntrypointEncSlice\n"
        )
        self.assertEqual(profiles["entries"], ["VAProfileH264Main:VAEntrypointEncSlice", "VAProfileH264Main:VAEntrypointVLD"])

    def test_process_classification_from_real_command_lines(self):
        classify = snapshot.classify_process
        self.assertEqual(classify(["/usr/libexec/vibeshine/vibeshine-host", "conf"]), "host")
        self.assertEqual(classify(["/usr/bin/bash", "/usr/libexec/vibeshine/vibeshine-machine-host", "run"]), "host")
        self.assertEqual(classify(["/usr/libexec/vibeshine/vibeshine-session-broker"]), "session_helper")
        self.assertEqual(classify(["/usr/bin/python3", "-I", "/usr/libexec/vibeshine/vibeshine-global-limiter.py"]), "session_helper")
        self.assertEqual(classify(["/home/chasep/.local/share/Steam/ubuntu12_32/steam", "-srt-logger-opened"]), "steam")
        self.assertEqual(classify(["/usr/lib/wine/../../bin/wineserver"]), "game")
        self.assertEqual(classify(["mangohud", "proton", "run", "game.exe"]), "game")
        self.assertEqual(classify(["/x/steamapps/common/Game/game.sh"]), "game")
        self.assertIsNone(classify(["/x/steamapps/common/SteamLinuxRuntime_sniper/pressure-vessel/bin/pv-adverb"]))
        self.assertIsNone(classify(["steamwebhelper", "--type=zygote"]))
        self.assertIsNone(classify(["/usr/bin/firefox"]))

    def test_process_capture_keeps_program_names_but_never_arguments(self):
        probe = FixtureProbe()
        probe.processes.append((777, "mangohud", 1, "1000", ["mangohud", "proton", "run", "--token=SECRETVALUE"]))
        probe.install_processes()
        state = capture(probe)
        self.assertNotIn("SECRETVALUE", json.dumps(state))
        groups = {item["pid"]: item["group"] for item in section(state, "processes")["processes"]}
        self.assertEqual(groups[5237], "host")
        self.assertEqual(groups[777], "game")
        self.assertNotIn(4238, groups)

    def test_malformed_evidence_becomes_unavailable_with_reason(self):
        for text in ("not json", "{}", '{"outputs": []}', '{"outputs": [{"name": "X"}]}', "[]"):
            with self.subTest(text=text):
                with self.assertRaises(snapshot.EvidenceError):
                    snapshot.parse_kscreen_json(text)
        probe = FixtureProbe(kscreen_json="")
        probe.kscreen_text = ""
        reading = capture(probe)["sections"]["displays"]
        self.assertEqual(reading["status"], "unavailable")
        self.assertIn("both failed", reading["reason"])
        probe = FixtureProbe(kscreen_json="not json")
        self.assertIn("not JSON", capture(probe)["sections"]["displays"]["reason"])
        probe = FixtureProbe(kscreen_json='{"outputs": [{"name": "Virtual-1", "connected": true, "enabled": true}]}')
        reading = capture(probe)["sections"]["displays"]
        self.assertIn("malformed evidence", reading["reason"])

    def test_malformed_pactl_output_is_unavailable(self):
        for attribute, text in (("sinks", "garbage"), ("sinks", '[{"description": "no name"}]'), ("modules", "{}")):
            with self.subTest(attribute=attribute, text=text):
                probe = FixtureProbe()
                setattr(probe, attribute, text)
                reading = capture(probe)["sections"]["audio"]
                self.assertEqual(reading["status"], "unavailable")
                self.assertIn("malformed evidence", reading["reason"])
        probe = FixtureProbe()
        probe.pactl_info = "Server Name: PulseAudio\n"
        self.assertEqual(capture(probe)["sections"]["audio"]["status"], "unavailable")

    def test_stat_record_and_acl_malformation_fail_closed(self):
        with self.assertRaises(snapshot.EvidenceError):
            snapshot.parse_stat_line("character special file\t660\n", "/dev/x")
        with self.assertRaises(snapshot.EvidenceError):
            snapshot.parse_getfacl("", "/dev/x")


class IdenticalAndDriftTests(unittest.TestCase):
    def setUp(self):
        self.baseline = capture()

    def compare(self, after: dict, owned=(), allow_owned_present=False):
        return snapshot.compare_states(self.baseline, after, owned, allow_owned_present)

    def assert_drift(self, comparison, area: str, key: str):
        drifted = [f for f in findings_for(comparison, area, key) if f.classification == snapshot.CLASS_DRIFT]
        self.assertTrue(drifted, f"no drift reported for {area} {key}: {comparison.findings}")
        self.assertEqual(comparison.exit_code, 1)

    def test_identical_snapshots_compare_clean(self):
        comparison = self.compare(copy.deepcopy(self.baseline))
        self.assertEqual(comparison.exit_code, 0)
        self.assertTrue(comparison.clean)
        restored = {(f.area, f.key) for f in comparison.by_class(snapshot.CLASS_RESTORED)}
        for protected in (("displays", "Virtual-1"), ("displays", "HDMI-A-1"), ("audio", "default_sink")):
            self.assertIn(protected, restored)
        self.assertFalse(comparison.by_class(snapshot.CLASS_DRIFT))

    def test_two_real_captures_of_the_same_fixture_host_are_clean(self):
        self.assertTrue(self.compare(capture()).clean)

    def test_changed_default_sink_is_drift(self):
        after = edit(self.baseline, lambda s: section(s, "audio").update(
            default_sink="alsa_output.pci-0000_00_1f.3.iec958-stereo"))
        comparison = self.compare(after)
        self.assert_drift(comparison, "audio", "default_sink")

    def test_disabled_monitor_becoming_enabled_is_drift(self):
        after = edit(self.baseline, lambda s: section(s, "displays")["outputs"]["HDMI-A-1"].update(enabled=True))
        self.assert_drift(self.compare(after), "displays", "HDMI-A-1")

    def test_changed_mode_refresh_scale_and_hdr_are_drift(self):
        changes = {"mode": "3024x1890", "refresh_hz": 90.0, "scale": 2.0, "hdr": False, "wcg": False,
                   "position": [100, 0], "priority": 2, "rotation": 2}
        for field, value in changes.items():
            with self.subTest(field=field):
                after = edit(self.baseline, lambda s: section(s, "displays")["outputs"]["Virtual-1"].update({field: value}))
                comparison = self.compare(after)
                self.assert_drift(comparison, "displays", "Virtual-1")
                detail = findings_for(comparison, "displays", "Virtual-1")[0].detail
                self.assertIn(field, detail)

    def test_drift_from_changed_command_output_end_to_end(self):
        document = json.loads(KSCREEN_JSON)
        document["outputs"][0]["scale"] = 1.5
        document["outputs"][0]["hdr"] = False
        comparison = self.compare(capture(FixtureProbe(json.dumps(document))))
        self.assert_drift(comparison, "displays", "Virtual-1")

    def test_a_removed_protected_output_is_drift(self):
        after = edit(self.baseline, lambda s: section(s, "displays")["outputs"].pop("Virtual-1"))
        comparison = self.compare(after)
        self.assert_drift(comparison, "displays", "Virtual-1")
        self.assertIn("protected", findings_for(comparison, "displays", "Virtual-1")[0].detail)

    def test_changed_device_acl_mode_and_access_are_drift(self):
        for field, value in (("acl", ["user::rw-", "other::---"]), ("mode", "666"), ("access", "--"), ("group", "root")):
            with self.subTest(field=field):
                after = edit(self.baseline, lambda s: section(s, "devices")["entries"]["/dev/uinput"].update({field: value}))
                self.assert_drift(self.compare(after), "devices", "/dev/uinput")

    def test_acl_drift_from_getfacl_output_end_to_end(self):
        probe = FixtureProbe()
        probe.acls["/dev/uinput"] = USER_ACL.replace("user:chasep:rw-\n", "")
        self.assert_drift(self.compare(capture(probe)), "devices", "/dev/uinput")

    def test_changed_vibeshine_host_pid_is_drift(self):
        probe = FixtureProbe()
        probe.processes = [p if p[0] != 5237 else (6001, p[1], p[2], "99999", p[4]) for p in PROCESS_FIXTURES]
        probe.install_processes()
        comparison = self.compare(capture(probe))
        self.assert_drift(comparison, "processes", "host:5237")
        self.assert_drift(comparison, "processes", "host:6001")

    def test_pid_reuse_with_a_new_start_time_is_not_the_same_process(self):
        after = edit(self.baseline, lambda s: [
            p.update(starttime="123456") for p in section(s, "processes")["processes"] if p["pid"] == 5237])
        self.assert_drift(self.compare(after), "processes", "host:5237")

    def test_new_game_process_is_drift_unless_its_pid_is_owned(self):
        probe = FixtureProbe()
        probe.processes = list(PROCESS_FIXTURES) + [(31000, "wineserver", 1, "5000", ["wineserver"])]
        probe.install_processes()
        after = capture(probe)
        self.assert_drift(self.compare(after), "processes", "game:31000")
        self.assertEqual(self.compare(after, owned=["31000"], allow_owned_present=True).exit_code, 0)

    def test_session_helper_churn_is_informational(self):
        probe = FixtureProbe()
        probe.processes = list(PROCESS_FIXTURES) + [
            (9100, "vibeshine-sessi", 5237, "9100", ["/usr/libexec/vibeshine/vibeshine-session-exec", "audio-capture"])]
        probe.install_processes()
        comparison = self.compare(capture(probe))
        self.assertEqual(comparison.exit_code, 0)
        self.assertTrue(findings_for(comparison, "processes", "session_helper"))

    def test_moved_playback_stream_is_drift(self):
        probe = FixtureProbe()
        probe.sink_inputs = SYNTHETIC_SINK_INPUTS
        baseline = capture(probe)
        after = edit(baseline, lambda s: section(s, "audio")["sink_inputs"]["4001"].update(
            sink="alsa_output.pci-0000_00_1f.3.iec958-stereo"))
        comparison = snapshot.compare_states(baseline, after)
        self.assert_drift(comparison, "audio.sink_inputs", "4001")

    def test_stream_start_and_end_are_ordinary_churn(self):
        probe = FixtureProbe()
        probe.sink_inputs = SYNTHETIC_SINK_INPUTS
        with_stream = capture(probe)
        self.assertEqual(snapshot.compare_states(self.baseline, with_stream).exit_code, 0)
        self.assertEqual(snapshot.compare_states(with_stream, self.baseline).exit_code, 0)

    def test_audio_module_sink_source_and_module_changes_are_drift(self):
        after = edit(self.baseline, lambda s: section(s, "audio")["modules"].append(
            {"name": "module-null-sink", "argument": "sink_name=stray"}))
        self.assertEqual(self.compare(after).exit_code, 1)
        after = edit(self.baseline, lambda s: section(s, "audio")["sinks"].pop("sink-sunshine-stereo"))
        self.assert_drift(self.compare(after), "audio.sinks", "sink-sunshine-stereo")
        after = edit(self.baseline, lambda s: section(s, "audio").update(default_source="other"))
        self.assert_drift(self.compare(after), "audio", "default_source")

    def test_advisory_sections_detect_drift(self):
        cases = (
            ("nvidia_encoder", lambda s: section(s, "nvidia_encoder")["gpus"][0].update(encoder_sessions=2), "GPU-fixture-uuid"),
            ("kernel_modules", lambda s: section(s, "kernel_modules")["modules"]["vibeshine_drm"].update(loaded=False, state=None), "vibeshine_drm"),
            ("dbus_names", lambda s: section(s, "dbus_names")["names"].update({"org.freedesktop.portal.Desktop": False}), "org.freedesktop.portal.Desktop"),
            ("listeners", lambda s: section(s, "listeners")["listeners"].append({"proto": "tcp", "address": "0.0.0.0", "port": 47990}), "tcp/0.0.0.0:47990"),
            ("drm_connectors", lambda s: section(s, "drm_connectors")["connectors"]["card1-HDMI-A-1"].update(status="disconnected"), "card1-HDMI-A-1"),
        )
        for area, mutate, key in cases:
            with self.subTest(area=area):
                self.assert_drift(self.compare(edit(self.baseline, mutate)), area, key)

    def test_disk_and_lock_are_informational_only(self):
        after = edit(self.baseline, lambda s: section(s, "disk")["free_bytes"].update({"/home": 1}))
        after["sections"]["native_lock"] = {"status": "ok", "data": {"state": "held_by_other", "path": "x", "holder_pid": 1}}
        self.assertEqual(self.compare(after).exit_code, 0)

    def test_baseline_from_another_host_is_an_evidence_error(self):
        after = copy.deepcopy(self.baseline)
        after["host"] = "elsewhere"
        with self.assertRaises(snapshot.EvidenceError):
            self.compare(after)

    def test_rebooted_host_is_reported(self):
        after = copy.deepcopy(self.baseline)
        after["boot_id"] = "other"
        self.assertTrue(findings_for(self.compare(after), "host", "boot_id"))


class OwnedResourceTests(unittest.TestCase):
    def setUp(self):
        self.baseline = capture()
        self.during = with_virtual_2(self.baseline)

    def test_appearing_virtual_2_is_drift_unless_named_owned(self):
        comparison = snapshot.compare_states(self.baseline, self.during)
        self.assertEqual(comparison.exit_code, 1)
        classes = {f.classification for f in findings_for(comparison, "displays", "Virtual-2")}
        self.assertEqual(classes, {snapshot.CLASS_DRIFT})

    def test_named_owned_virtual_2_may_appear_when_allowed(self):
        comparison = snapshot.compare_states(self.baseline, self.during, ["Virtual-2"], allow_owned_present=True)
        self.assertEqual(comparison.exit_code, 0)
        self.assertEqual(findings_for(comparison, "displays", "Virtual-2")[0].classification, snapshot.CLASS_OWNED_PRESENT)
        self.assertEqual(findings_for(comparison, "drm_connectors", "card2-Virtual-2")[0].classification, snapshot.CLASS_OWNED_PRESENT)

    def test_named_owned_virtual_2_still_present_is_not_restored_by_default(self):
        comparison = snapshot.compare_states(self.baseline, self.during, ["Virtual-2"])
        self.assertEqual(comparison.exit_code, 1)
        self.assertEqual(comparison.by_class(snapshot.CLASS_OWNED_PRESENT)[0].blocking, True)

    def test_owned_virtual_2_disappearing_passes_only_when_named(self):
        unnamed = snapshot.compare_states(self.during, self.baseline)
        self.assertEqual(unnamed.exit_code, 1)
        named = snapshot.compare_states(self.during, self.baseline, ["Virtual-2"])
        self.assertEqual(named.exit_code, 0)
        self.assertEqual(findings_for(named, "displays", "Virtual-2")[0].classification, snapshot.CLASS_OWNED_GONE)
        self.assertEqual(findings_for(named, "drm_connectors", "card2-Virtual-2")[0].classification, snapshot.CLASS_OWNED_GONE)

    def test_naming_virtual_2_does_not_excuse_other_drift(self):
        after = edit(self.baseline, lambda s: section(s, "displays")["outputs"]["Virtual-1"].update(scale=1.0))
        comparison = snapshot.compare_states(self.baseline, after, ["Virtual-2"])
        self.assertEqual(comparison.exit_code, 1)

    def test_owned_sink_with_monitor_module_and_stream_is_gone_when_named(self):
        probe = FixtureProbe()
        probe.sink_inputs = SYNTHETIC_SINK_INPUTS
        baseline = capture(probe)
        during = with_owned_sink(baseline)
        self.assertEqual(snapshot.compare_states(baseline, during).exit_code, 1)
        self.assertEqual(snapshot.compare_states(baseline, during, ["amaterasu_qual_42"]).exit_code, 1)
        allowed = snapshot.compare_states(baseline, during, ["amaterasu_qual_42"], allow_owned_present=True)
        self.assertEqual(allowed.exit_code, 0)
        gone = snapshot.compare_states(during, baseline, ["amaterasu_qual_42"])
        self.assertEqual(gone.exit_code, 0)
        gone_areas = {f.area for f in gone.by_class(snapshot.CLASS_OWNED_GONE)}
        self.assertTrue({"audio.sinks", "audio.sources", "audio.modules"} <= gone_areas)
        self.assertEqual(snapshot.compare_states(during, baseline).exit_code, 1)

    def test_default_sink_pointing_at_the_owned_sink_is_never_excused(self):
        during = with_owned_sink(self.baseline, move_input=False)
        section(during, "audio")["default_sink"] = "amaterasu_qual_42"
        comparison = snapshot.compare_states(self.baseline, during, ["amaterasu_qual_42"], allow_owned_present=True)
        self.assertEqual(comparison.exit_code, 1)
        self.assertEqual(findings_for(comparison, "audio", "default_sink")[0].classification, snapshot.CLASS_DRIFT)

    def test_off_limits_names_are_usage_errors(self):
        default_sink = section(self.baseline, "audio")["default_sink"]
        for name in ("Virtual-1", "HDMI-A-1", default_sink, "sink-sunshine-surround51", "alsa_output.usb-x", "Virtual-0", "foo", "0", ""):
            with self.subTest(name=name):
                with self.assertRaises(snapshot.UsageError):
                    snapshot.compare_states(self.baseline, self.baseline, [name])
        for name in ("Virtual-2", "Virtual-4", "amaterasu_qual_7", "amaterasu_stream_5_2ch", "4242"):
            self.assertEqual(snapshot.validate_owned_names([name]), frozenset([name]))


class UnavailableEvidenceTests(unittest.TestCase):
    def test_missing_tool_is_reported_unavailable_with_reason(self):
        probe = FixtureProbe()
        probe.missing_tools = {"kscreen-doctor", "pactl", "nvidia-smi", "getfacl"}
        state = capture(probe)
        for name in ("displays", "audio", "devices", "nvidia_encoder"):
            reading = state["sections"][name]
            self.assertEqual(reading["status"], "unavailable", name)
            self.assertIn("not found", reading["reason"])
        self.assertEqual(state["sections"]["processes"]["status"], "ok")

    def test_two_unavailable_required_readings_never_count_as_restored(self):
        probe = FixtureProbe()
        probe.missing_tools = {"kscreen-doctor"}
        one, two = capture(probe), capture(probe)
        comparison = snapshot.compare_states(one, two)
        self.assertEqual(comparison.exit_code, 2)
        self.assertFalse(comparison.clean)
        self.assertEqual(findings_for(comparison, "displays")[0].classification, snapshot.CLASS_UNVERIFIED)
        self.assertFalse(findings_for(comparison, "displays", "Virtual-1"))
        self.assertNotIn("displays", {f.area for f in comparison.by_class(snapshot.CLASS_RESTORED)})

    def test_one_side_unavailable_never_passes(self):
        probe = FixtureProbe()
        probe.missing_tools = {"pactl"}
        degraded, healthy = capture(probe), capture()
        for before, after in ((degraded, healthy), (healthy, degraded)):
            self.assertEqual(snapshot.compare_states(before, after).exit_code, 2)

    def test_drift_outranks_unavailable_evidence(self):
        probe = FixtureProbe()
        probe.missing_tools = {"pactl"}
        degraded = capture(probe)
        changed = edit(degraded, lambda s: section(s, "displays")["outputs"]["Virtual-1"].update(scale=1.0))
        self.assertEqual(snapshot.compare_states(degraded, changed).exit_code, 1)

    def test_advisory_unavailable_on_both_sides_is_listed_but_not_restored(self):
        baseline = capture()
        comparison = snapshot.compare_states(baseline, capture())
        vaapi = findings_for(comparison, "vaapi")
        self.assertEqual([f.classification for f in vaapi], [snapshot.CLASS_UNVERIFIED])
        self.assertFalse(vaapi[0].blocking)
        self.assertEqual(comparison.exit_code, 0)

    def test_advisory_unavailable_on_one_side_blocks(self):
        probe = FixtureProbe()
        probe.missing_tools = {"nvidia-smi"}
        self.assertEqual(snapshot.compare_states(capture(), capture(probe)).exit_code, 2)

    def test_malformed_state_fails_closed(self):
        good = capture()
        malformed = {
            "not an object": [],
            "wrong schema": dict(good, schema_version=99),
            "no sections": {"schema_version": 1},
            "missing section": edit(good, lambda s: s["sections"].pop("devices")),
            "bad status": edit(good, lambda s: s["sections"]["audio"].update(status="maybe")),
            "unavailable without reason": edit(good, lambda s: s["sections"]["audio"].update(status="unavailable", reason="")),
            "ok without data": edit(good, lambda s: s["sections"]["audio"].pop("data")),
            "ok data missing keys": edit(good, lambda s: section(s, "audio").pop("sinks")),
        }
        for label, state in malformed.items():
            with self.subTest(label=label):
                with self.assertRaises(snapshot.EvidenceError):
                    snapshot.compare_states(good, state)
                with self.assertRaises(snapshot.EvidenceError):
                    snapshot.compare_states(state, good)

    def test_malformed_section_contents_fail_closed(self):
        good = capture()
        broken = edit(good, lambda s: section(s, "displays")["outputs"].update({"Virtual-9": {"connected": True}}))
        with self.assertRaises(snapshot.EvidenceError):
            snapshot.compare_states(good, broken)


class PrivatePersistenceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def run_main(self, argv, state=None, capture_fn=None):
        output, errors = io.StringIO(), io.StringIO()
        capture_fn = capture_fn or (lambda: copy.deepcopy(state))
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
            code = snapshot.main(argv, capture=capture_fn)
        return code, output.getvalue(), errors.getvalue()

    def test_snapshot_creates_private_directory_and_file(self):
        target = self.root / "baselines" / "run1.json"
        code, out, _ = self.run_main(["snapshot", "--out", str(target)], capture())
        self.assertEqual(code, 0, out)
        self.assertEqual(stat.S_IMODE(target.parent.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        document = json.loads(target.read_text())
        self.assertEqual(document["host"], "fixture-host")
        self.assertRegex(document["captured_at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual(document["schema_version"], 1)
        self.assertFalse(list(target.parent.glob(".*.tmp")), "temporary file left behind")

    def test_umask_cannot_widen_permissions(self):
        previous = os.umask(0)
        self.addCleanup(os.umask, previous)
        target = self.root / "open" / "run.json"
        self.assertEqual(self.run_main(["snapshot", "--out", str(target)], capture())[0], 0)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(target.parent.stat().st_mode), 0o700)

    def test_snapshot_refuses_a_shared_directory_and_existing_file(self):
        shared = self.root / "shared"
        shared.mkdir(mode=0o755)
        shared.chmod(0o755)
        code, _, errors = self.run_main(["snapshot", "--out", str(shared / "b.json")], capture())
        self.assertEqual(code, 2)
        self.assertIn("private", errors)
        private = self.root / "private"
        self.assertEqual(self.run_main(["snapshot", "--out", str(private / "b.json")], capture())[0], 0)
        code, _, errors = self.run_main(["snapshot", "--out", str(private / "b.json")], capture())
        self.assertEqual(code, 2)
        self.assertIn("already exists", errors)

    def test_snapshot_with_unavailable_required_evidence_writes_nothing(self):
        probe = FixtureProbe()
        probe.missing_tools = {"pactl"}
        target = self.root / "p" / "b.json"
        code, _, errors = self.run_main(["snapshot", "--out", str(target)], capture(probe))
        self.assertEqual(code, 2)
        self.assertIn("audio", errors)
        self.assertFalse(target.exists())

    def test_compare_cli_exit_codes(self):
        baseline = capture()
        target = self.root / "d" / "b.json"
        self.run_main(["snapshot", "--out", str(target)], baseline)
        self.assertEqual(self.run_main(["compare", "--baseline", str(target)], baseline)[0], 0)
        drifted = edit(baseline, lambda s: section(s, "displays")["outputs"]["HDMI-A-1"].update(enabled=True))
        code, out, _ = self.run_main(["compare", "--baseline", str(target)], drifted)
        self.assertEqual(code, 1)
        self.assertIn("NOT RESTORED", out)
        probe = FixtureProbe()
        probe.missing_tools = {"kscreen-doctor"}
        self.assertEqual(self.run_main(["compare", "--baseline", str(target)], capture(probe))[0], 2)

    def test_compare_cli_json_report(self):
        baseline = capture()
        target = self.root / "j" / "b.json"
        self.run_main(["snapshot", "--out", str(target)], baseline)
        code, out, _ = self.run_main(["compare", "--baseline", str(target), "--json"], baseline)
        report = json.loads(out)
        self.assertEqual((code, report["exit_code"]), (0, 0))
        self.assertTrue(any(f["classification"] == "restored" for f in report["findings"]))

    def test_compare_cli_owned_names(self):
        baseline = capture()
        target = self.root / "o" / "b.json"
        self.run_main(["snapshot", "--out", str(target)], baseline)
        during = with_virtual_2(baseline)
        self.assertEqual(self.run_main(["compare", "--baseline", str(target), "--owned", "Virtual-2"], during)[0], 1)
        argv = ["compare", "--baseline", str(target), "--owned", "Virtual-2", "--allow-owned-present"]
        self.assertEqual(self.run_main(argv, during)[0], 0)
        code, _, errors = self.run_main(["compare", "--baseline", str(target), "--owned", "Virtual-1"], baseline)
        self.assertEqual(code, 2)
        self.assertIn("cannot be owned", errors)

    def test_missing_malformed_and_wrong_baselines_exit_2(self):
        good = capture()
        directory = self.root / "m"
        directory.mkdir(mode=0o700)
        cases = {
            "missing.json": None,
            "empty.json": "",
            "truncated.json": json.dumps(good)[:200],
            "array.json": "[]",
            "schema.json": json.dumps(dict(good, schema_version=2)),
        }
        for name, content in cases.items():
            with self.subTest(name=name):
                if content is not None:
                    (directory / name).write_text(content)
                code, _, errors = self.run_main(["compare", "--baseline", str(directory / name)], good)
                self.assertEqual(code, 2)
                self.assertIn("error:", errors)

    def test_usage_errors_exit_2(self):
        self.assertEqual(self.run_main([], capture())[0], 2)
        self.assertEqual(self.run_main(["compare"], capture())[0], 2)
        self.assertEqual(self.run_main(["bogus"], capture())[0], 2)


class WaitRestoredTests(unittest.TestCase):
    class Clock:
        def __init__(self):
            self.now = 0.0

        def monotonic(self):
            return self.now

        def sleep(self, seconds):
            self.now += seconds

    def setUp(self):
        self.baseline = capture()
        self.drifted = edit(self.baseline, lambda s: section(s, "displays")["outputs"]["HDMI-A-1"].update(enabled=True))
        self.clock = self.Clock()

    def wait(self, captures, deadline=10.0, **kwargs):
        sequence = iter(captures)
        last = [None]

        def capture_next():
            last[0] = next(sequence, last[0])
            return last[0]

        return snapshot.wait_restored(
            self.baseline, kwargs.pop("owned", ()), deadline, capture=capture_next,
            sleep=self.clock.sleep, monotonic=self.clock.monotonic, poll_interval=3.0, **kwargs)

    def test_expires_with_exit_1_at_its_deadline(self):
        comparison = self.wait([self.drifted])
        self.assertEqual(comparison.exit_code, 1)
        self.assertEqual(self.clock.now, 10.0)

    def test_returns_as_soon_as_the_host_is_restored(self):
        comparison = self.wait([self.drifted, self.drifted, self.baseline])
        self.assertEqual(comparison.exit_code, 0)
        self.assertEqual(self.clock.now, 6.0)

    def test_zero_wait_checks_once(self):
        self.assertEqual(self.wait([self.drifted], deadline=0).exit_code, 1)
        self.assertEqual(self.clock.now, 0.0)

    def test_unavailable_evidence_at_the_deadline_is_exit_2(self):
        probe = FixtureProbe()
        probe.missing_tools = {"kscreen-doctor"}
        comparison = self.wait([capture(probe)], deadline=4.0)
        self.assertEqual(comparison.exit_code, 2)

    def test_waits_for_an_owned_output_to_be_released(self):
        during = with_virtual_2(self.baseline)
        comparison = self.wait([during, during, self.baseline], owned=["Virtual-2"])
        self.assertEqual(comparison.exit_code, 0)

    def test_cli_wait_restored_expires_with_exit_1(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "b.json"
            snapshot.write_private_json(str(target), self.baseline)
            output, errors = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                code = snapshot.main(
                    ["compare", "--baseline", str(target), "--wait-restored", "0.4", "--poll-interval", "0.1"],
                    capture=lambda: copy.deepcopy(self.drifted))
        self.assertEqual(code, 1)
        self.assertIn("not restored", errors.getvalue())


class ReadOnlyGuardTests(unittest.TestCase):
    ALLOWED = (
        ["kscreen-doctor", "-j"],
        ["kscreen-doctor", "-o"],
        ["pactl", "info"],
        ["pactl", "-f", "json", "list", "sinks"],
        ["pactl", "-f", "json", "list", "sources"],
        ["pactl", "-f", "json", "list", "modules"],
        ["pactl", "-f", "json", "list", "sink-inputs"],
        ["getfacl", "-cp", "/dev/uinput"],
        ["stat", r"--printf=%F\t%a\t%U\t%G\t%t:%T\n", "/dev/dri/card0"],
        ["nvidia-smi", "--query-gpu=index,uuid,name,encoder.stats.sessionCount", "--format=csv,noheader,nounits"],
        ["vainfo"],
    )
    MUTATING = (
        ["kscreen-doctor", "output.Virtual-2.disable"],
        ["kscreen-doctor", "output.HDMI-A-1.enable", "output.Virtual-1.scale.1"],
        ["kscreen-doctor", "-j", "output.Virtual-1.mode.1"],
        ["kscreen-doctor", "--dpms", "off"],
        ["pactl", "set-default-sink", "x"],
        ["pactl", "set-sink-volume", "x", "0"],
        ["pactl", "load-module", "module-null-sink"],
        ["pactl", "unload-module", "1"],
        ["pactl", "move-sink-input", "1", "2"],
        ["pactl", "suspend-sink", "x", "1"],
        ["pactl", "-f", "json", "list", "sinks", "extra"],
        ["pactl", "subscribe"],
        ["nvidia-smi", "-pm", "1"],
        ["nvidia-smi", "-r"],
        ["nvidia-smi", "--query-gpu=index", "--format=csv", "-e", "0"],
        ["nvidia-smi", "--gpu-reset"],
        ["getfacl", "-R", "/dev"],
        ["getfacl", "-cp", "relative"],
        ["setfacl", "-m", "u:x:rw", "/dev/uinput"],
        ["stat", "-f", "/dev/uinput"],
        ["chmod", "600", "/dev/uinput"],
        ["rm", "-rf", "/"],
        ["sudo", "pactl", "info"],
        ["flock", "x", "true"],
        ["sh", "-c", "pactl info"],
        [],
    )

    def test_allowlist_accepts_only_read_only_invocations(self):
        for argv in self.ALLOWED:
            with self.subTest(argv=argv):
                snapshot.validate_argv(argv)
        for argv in self.MUTATING:
            with self.subTest(argv=argv):
                with self.assertRaises(snapshot.UsageError):
                    snapshot.validate_argv(argv)

    def test_a_full_capture_runs_only_allowlisted_argv_without_mutating_tokens(self):
        probe = FixtureProbe()
        capture(probe)
        self.assertTrue(probe.argv_log)
        tools = {os.path.basename(argv[0]) for argv in probe.argv_log}
        self.assertEqual(tools, {"kscreen-doctor", "pactl", "stat", "getfacl", "nvidia-smi"})
        for argv in probe.argv_log:
            snapshot.validate_argv(argv)
            for token in argv:
                self.assertFalse(token.startswith(("output.", "set-", "load-", "unload-", "move-")), argv)

    def test_the_real_runner_refuses_before_spawning(self):
        with mock.patch.object(subprocess, "run") as run:
            with self.assertRaises(snapshot.UsageError):
                snapshot.run_readonly(["pactl", "set-default-sink", "x"])
            run.assert_not_called()

    def test_real_runner_reports_missing_tool_as_unavailable(self):
        with mock.patch.object(subprocess, "run", side_effect=FileNotFoundError):
            with self.assertRaises(snapshot.ToolUnavailable):
                snapshot.run_readonly(["kscreen-doctor", "-j"])
        with mock.patch.object(subprocess, "run", side_effect=subprocess.TimeoutExpired("x", 1)):
            with self.assertRaises(snapshot.ToolUnavailable):
                snapshot.run_readonly(["pactl", "info"])

    def test_source_has_no_mutating_command_strings_and_one_subprocess_call(self):
        tree = ast.parse(SCRIPT.read_text())
        forbidden = re.compile(r"^(output\.|set-|load-|unload-|move-|suspend-|kill-)")
        for node in ast.walk(tree):
            if isinstance(node, ast.Constant) and isinstance(node.value, str):
                self.assertIsNone(forbidden.match(node.value), f"mutating token in source: {node.value!r}")
        calls = [
            node for node in ast.walk(tree)
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name) and node.func.value.id == "subprocess"
        ]
        self.assertEqual([call.func.attr for call in calls], ["run"], "subprocess must only be used by run_readonly")
        banned = {"system", "popen", "remove", "rename", "chmod_device", "setxattr", "kill", "killpg", "mknod"}
        for node in ast.walk(tree):
            if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name) and node.value.id == "os":
                self.assertNotIn(node.attr, banned, f"os.{node.attr} is not allowed in a read-only tool")

    def test_devices_are_never_opened(self):
        tree = ast.parse(SCRIPT.read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "open":
                target = ast.unparse(node.args[0]) if node.args else ""
                self.assertNotIn("/dev", target)
        for pattern in snapshot.DEVICE_PATTERNS:
            self.assertTrue(pattern.startswith(("/dev/", "/run/")))


class FakeSessionBus(threading.Thread):
    """Just enough D-Bus daemon for Hello and NameHasOwner."""

    def __init__(self, path: str, owners: dict[str, bool]):
        super().__init__(daemon=True)
        self.owners = owners
        self.requests: list[str] = []
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(path)
        self.server.listen(1)

    def reply(self, connection, reply_serial, signature, body):
        fields = bytearray(struct.pack("<BBBBI", 5, 1, ord("u"), 0, reply_serial))
        fields.extend(bytes([8, 1, ord("g"), 0, len(signature)]) + signature.encode() + b"\0")
        message = bytearray(b"l\x02\x01\x01") + struct.pack("<II", len(body), 99) + struct.pack("<I", len(fields)) + fields
        message.extend(b"\0" * (-len(message) % 8))
        connection.sendall(bytes(message) + body)

    def run(self):
        connection, _ = self.server.accept()
        connection.settimeout(5)
        reader = connection.makefile("rb")
        reader.readline()  # NUL AUTH EXTERNAL <hex uid>
        connection.sendall(b"OK 00112233445566778899aabbccddeeff\r\n")
        reader.readline()  # BEGIN
        for _ in range(len(self.owners) + 1):
            fixed = reader.read(16)
            body_length, serial, fields_length = struct.unpack("<III", fixed[4:16])
            fields = reader.read(fields_length)
            reader.read(-fields_length % 8)
            body = reader.read(body_length)
            if b"Hello" in fields:
                self.reply(connection, serial, "s", struct.pack("<I", 5) + b":1.42\0")
            else:
                (length,) = struct.unpack("<I", body[:4])
                name = body[4 : 4 + length].decode()
                self.requests.append(name)
                self.reply(connection, serial, "b", struct.pack("<I", int(self.owners[name])))
        reader.close()
        connection.close()
        self.server.close()


class DBusTests(unittest.TestCase):
    def test_name_has_owner_against_a_fake_bus(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "bus")
            owners = {"org.freedesktop.portal.Desktop": True, "org.pulseaudio.Server": False}
            bus = FakeSessionBus(path, owners)
            bus.start()
            with mock.patch.dict(os.environ, {"DBUS_SESSION_BUS_ADDRESS": f"unix:path={path},guid=abc"}):
                result = snapshot.dbus_name_has_owner(list(owners))
            bus.join(5)
        self.assertEqual(result, owners)
        self.assertEqual(bus.requests, list(owners))

    def test_unreachable_bus_is_unavailable_not_an_error(self):
        with mock.patch.dict(os.environ, {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent/amaterasu-bus"}):
            with self.assertRaises(snapshot.ToolUnavailable):
                snapshot.dbus_name_has_owner(["x"])

    def test_method_call_encoding_is_aligned(self):
        message = snapshot.encode_dbus_method_call(
            2, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", "a.b")
        fields_length = struct.unpack("<I", message[12:16])[0]
        body_offset = 16 + fields_length + (-fields_length % 8)
        self.assertEqual(message[body_offset:], struct.pack("<I", 3) + b"a.b\0")
        self.assertEqual(struct.unpack("<I", message[4:8])[0], 8)


class NativeLockTests(unittest.TestCase):
    """Uses a private temporary file, never the real native lock."""

    def probe_for(self, path):
        probe = snapshot.HostProbe()
        probe.native_lock_path = path
        return probe

    def test_absent_free_and_held_states(self):
        import fcntl

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "native.lock")
            self.assertEqual(snapshot.capture_native_lock(self.probe_for(path))["state"], "absent")
            Path(path).touch()
            self.assertEqual(snapshot.capture_native_lock(self.probe_for(path))["state"], "free")
            with open(path) as ours:
                fcntl.flock(ours, fcntl.LOCK_EX)
                reading = snapshot.capture_native_lock(self.probe_for(path))
                self.assertEqual(reading["state"], "held_by_this_process_tree")
                self.assertEqual(reading["holder_pid"], os.getpid())
            self.assertEqual(snapshot.capture_native_lock(self.probe_for(path))["state"], "free")
            holder = subprocess.Popen(
                [sys.executable, "-c",
                 "import fcntl,sys,time\nf=open(sys.argv[1])\nfcntl.flock(f,fcntl.LOCK_EX)\nprint('locked',flush=True)\ntime.sleep(60)",
                 path],
                stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(holder.stdout.readline().strip(), "locked")
                reading = snapshot.capture_native_lock(self.probe_for(path))
                self.assertEqual((reading["state"], reading["holder_pid"]), ("held_by_other", holder.pid))
            finally:
                holder.terminate()
                holder.wait(10)
                holder.stdout.close()

    def test_btrfs_style_device_mismatch_is_still_detected(self):
        locks = (
            "1: FLOCK  ADVISORY  WRITE 111 00:21:777 0 EOF\n"
            "2: FLOCK  ADVISORY  WRITE 222 00:21:778 0 EOF\n"
            "3: POSIX  ADVISORY  WRITE 333 00:21:777 0 EOF\n"
        )
        holders = lambda confirm: snapshot._flock_holders(locks, "00:2a", 777, confirm)
        self.assertEqual(holders(lambda pid: True), [111])
        self.assertEqual(holders(lambda pid: None), [111], "unreadable holders are assumed to hold it")
        self.assertEqual(holders(lambda pid: False), [], "a readable holder without the file open is another file")
        self.assertEqual(snapshot._flock_holders(locks, "00:21", 777, lambda pid: False), [111])

    def test_the_lock_is_inspected_never_taken(self):
        tree = ast.parse(SCRIPT.read_text())
        for node in ast.walk(tree):
            if isinstance(node, ast.Attribute):
                self.assertNotIn(node.attr, {"flock", "lockf"})


class PreflightTests(unittest.TestCase):
    def evaluate(self, probe=None):
        state = capture(probe)
        checks = snapshot.evaluate_preflight(state)
        return state, {check.name: check for check in checks}, snapshot.preflight_exit_code(state, checks)

    def test_fixture_host_preflight(self):
        _state, checks, code = self.evaluate()
        self.assertEqual(code, 0)
        self.assertEqual(checks["display"].status, "pass")
        self.assertIn("Virtual-1 connected enabled 3840x2160@120.0 scale 2.5 hdr=True wcg=True", checks["display"].detail)
        self.assertIn("Virtual-2", checks["display"].detail)
        self.assertEqual(checks["vkms-helper-socket"].status, "pending")
        self.assertEqual(checks["privilege"].status, "pending")
        self.assertEqual(checks["vibeshine-host"].status, "pass")
        self.assertEqual(checks["native-lock"].status, "warn")
        self.assertEqual(checks["evidence:vaapi"].status, "unavailable")

    def test_low_disk_fails_and_stale_amaterasu_sink_fails(self):
        probe = FixtureProbe()
        probe.free_bytes["/home"] = 11 * 1024**3
        self.assertEqual(self.evaluate(probe)[2], 1)
        stale = FixtureProbe()
        stale.sinks = PACTL_SINKS.rstrip()[:-1] + ',{"index": 9, "name": "amaterasu_qual_1", "description": "AmaterasuAudio", "owner_module": 7}]'
        _, checks, code = self.evaluate(stale)
        self.assertEqual((checks["audio"].status, code), ("fail", 1))

    def test_unavailable_required_evidence_exits_2(self):
        probe = FixtureProbe()
        probe.missing_tools = {"kscreen-doctor"}
        probe.kscreen_json = probe.kscreen_text = ""
        state, checks, code = self.evaluate(probe)
        self.assertEqual(code, 2)
        self.assertEqual(checks["display"].status, "unavailable")

    def test_preflight_cli_json_and_text(self):
        for flags in ([], ["--json"]):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = snapshot.main(["preflight", *flags], capture=capture)
            self.assertEqual(code, 0)
            if flags:
                report = json.loads(output.getvalue())
                self.assertEqual(report["exit_code"], 0)
                self.assertIn("sections", report["state"])
            else:
                self.assertIn("no milestone is claimed", output.getvalue())


if __name__ == "__main__":
    unittest.main(verbosity=1)
