#!/usr/bin/env python3
"""Bounded, read-only Linux host scheduling and socket-pressure recorder."""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import time

MAX_READ = 1024 * 1024


def read_text(path: Path) -> str:
    with path.open(encoding="utf-8", errors="replace") as stream:
        return stream.read(MAX_READ)


def process_identity(stat: str) -> tuple[int, int]:
    # comm may contain spaces and parentheses; fields after its final ')' are fixed.
    end = stat.rfind(")")
    values = stat[end + 2:].split()
    if end < 0 or len(values) < 20:
        raise ValueError("incomplete process stat")
    return int(stat.split("(", 1)[0]), int(values[19])


def parse_pairs(text: str) -> dict:
    result = {}
    for line in text.splitlines():
        key, separator, value = line.partition(":")
        if separator:
            fields = value.split()
            if fields and fields[0].isdigit():
                result[key] = int(fields[0])
    return result


def pressure(text: str) -> dict:
    result = {}
    for line in text.splitlines():
        values = line.split()
        if values:
            result[values[0]] = {k: float(v) if k.startswith("avg") else int(v)
                                for k, v in (item.split("=", 1) for item in values[1:])}
    return result


def sockets(text: str, owned: set[str]) -> list[dict]:
    result = []
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) < 13 or fields[9] not in owned:
            continue
        transmit, receive = fields[4].split(":")
        result.append({"local_port": int(fields[1].split(":")[1], 16),
                       "inode": fields[9], "send_queue_bytes": int(transmit, 16),
                       "receive_queue_bytes": int(receive, 16), "receive_drops_cumulative": int(fields[-1])})
    return result


def counters(text: str) -> dict:
    result = {}
    lines = text.splitlines()
    for header, values in zip(lines[::2], lines[1::2]):
        keys, counts = header.split(), values.split()
        if not keys or not counts or keys[0] != counts[0]:
            continue
        protocol = keys[0].rstrip(":")
        if protocol in ("Udp", "Ip", "Tcp"):
            result[protocol] = {key: int(value) for key, value in zip(keys[1:], counts[1:])
                                if key in ("InErrors", "RcvbufErrors", "SndbufErrors", "InCsumErrors",
                                           "InDiscards", "OutDiscards", "RetransSegs")}
    return result


def numeric_delta(current: dict, previous: dict | None) -> dict:
    result = {}
    if previous is None:
        return result
    for key, value in current.items():
        old = previous.get(key)
        if isinstance(value, dict) and isinstance(old, dict):
            result[key] = numeric_delta(value, old)
        elif isinstance(value, int) and not isinstance(value, bool) and isinstance(old, int):
            result[key] = value - old if value >= old else None
    return result


def optional(path: Path, parser=read_text):
    try:
        return parser(path)
    except (OSError, ValueError, IndexError):
        return None


def sample(proc: Path) -> dict:
    owned = set()
    for fd in (proc / "fd").iterdir():
        try:
            link = fd.readlink().as_posix()
            if link.startswith("socket:["):
                owned.add(link[8:-1])
        except OSError:
            continue
    threads = {}
    for task in (proc / "task").iterdir():
        try:
            run_ns, wait_ns, slices = map(int, read_text(task / "schedstat").split())
            status = parse_pairs(read_text(task / "status"))
            threads[task.name] = {"name": read_text(task / "comm").strip(), "kernel_wait_site": optional(task / "wchan"), "run_ns": run_ns,
                                  "runnable_wait_ns": wait_ns, "timeslices": slices,
                                  "voluntary_context_switches": status.get("voluntary_ctxt_switches"),
                                  "involuntary_context_switches": status.get("nonvoluntary_ctxt_switches")}
        except (OSError, ValueError):
            continue
    queue = []
    for name in ("udp", "udp6"):
        value = optional(proc / "net" / name, lambda path: sockets(read_text(path), owned))
        if value is not None:
            queue.extend(value)
    softnet = optional(Path("/proc/net/softnet_stat"))
    drops = squeeze = 0
    if softnet is not None:
        for line in softnet.splitlines():
            values = line.split()
            if len(values) >= 3:
                drops += int(values[1], 16)
                squeeze += int(values[2], 16)
    return {"threads": threads, "udp_sockets": queue,
            "process_io": optional(proc / "io", lambda path: parse_pairs(read_text(path))),
            "process_status": optional(proc / "status", lambda path: parse_pairs(read_text(path))),
            "pressure": {name: optional(Path("/proc/pressure") / name, lambda path: pressure(read_text(path)))
                         for name in ("cpu", "io", "memory")},
            "network_namespace_counters": optional(proc / "net/snmp", lambda path: counters(read_text(path))),
            "softnet_host_cumulative": None if softnet is None else {"dropped": drops, "time_squeeze": squeeze}}


class RingWriter:
    def __init__(self, path: Path, segment_bytes: int, segments: int = 4):
        self.path, self.segment_bytes, self.segments = path, segment_bytes, segments
        self.stream = None
        self.size = 0
        self._open()

    def _open(self):
        # Only the requested evidence file is created; no ownership/mode changes to parents.
        fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        self.stream = os.fdopen(fd, "w", encoding="utf-8")
        self.size = 0

    def write(self, record: dict):
        line = json.dumps(record, separators=(",", ":")) + "\n"
        size = len(line.encode())
        if size > self.segment_bytes:
            raise ValueError("sample exceeds segment budget")
        if self.size + size > self.segment_bytes:
            self.stream.close()
            oldest = self.path.with_name(self.path.name + f".{self.segments-1}")
            oldest.unlink(missing_ok=True)
            for index in range(self.segments - 2, 0, -1):
                source = self.path.with_name(self.path.name + f".{index}")
                if source.exists():
                    source.rename(self.path.with_name(self.path.name + f".{index+1}"))
            self.path.rename(self.path.with_name(self.path.name + ".1"))
            self._open()
        self.stream.write(line)
        self.stream.flush()
        self.size += size

    def close(self):
        self.stream.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--duration", type=float, default=1800)
    parser.add_argument("--interval", type=float, default=1)
    args = parser.parse_args()
    if args.pid < 1 or not 1 <= args.duration <= 7200 or not 0.25 <= args.interval <= 60:
        parser.error("PID, duration or sampling interval out of bounds")
    proc = Path("/proc") / str(args.pid)
    identity = process_identity(read_text(proc / "stat"))
    recorder = RingWriter(args.output, 2 * 1024 * 1024)
    started = time.monotonic()
    previous = None
    previous_at = None
    try:
        while time.monotonic() - started < args.duration:
            if process_identity(read_text(proc / "stat")) != identity:
                break
            at = time.monotonic_ns()
            observed = sample(proc)
            if process_identity(read_text(proc / "stat")) != identity:
                break
            elapsed_us = (time.monotonic_ns() - at) // 1000
            record = {"event": "host_observer_sample", "schema": 1, "pid": args.pid,
                      "process_start_ticks": identity[1], "timestamp": datetime.now(timezone.utc).isoformat(),
                      "monotonic_ns": at, "sample_us": elapsed_us,
                      "interval_us": None if previous_at is None else (at - previous_at) // 1000,
                      "observation": observed, "delta": numeric_delta(observed, previous),
                      "scope": "linux_host_counters_since_prior_sample_not_individual_packet_or_game_latency"}
            recorder.write(record)
            previous, previous_at = observed, at
            deadline = min(started + args.duration, at / 1e9 + args.interval)
            time.sleep(max(0, deadline - time.monotonic()))
    except (OSError, ValueError) as error:
        recorder.write({"event": "host_observer_stopped", "reason": type(error).__name__})
    finally:
        recorder.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
