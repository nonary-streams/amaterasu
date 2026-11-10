#!/usr/bin/env python3
"""Summarize recent host timing windows without connecting to or changing a host."""
from __future__ import annotations

import argparse
from collections import deque
import json
from pathlib import Path
import re
import sys

TARGET = re.compile(
    r"(?:^|: |::)(module\.timing|input\.timing|control\.timing|stream\.timing|"
    r"video\.pipeline|encoder\.timing|media\.processing_latency|capture\.presentation|service\.worker|broker\.timing|runtime\.invocation|audio\.timing|host\.timing|rtx-hdr): (.*)$"
)
FIELD = re.compile(r"([a-zA-Z_][a-zA-Z_0-9]*)=")
DECODER = json.JSONDecoder()


def parse_fields(message: str) -> dict:
    """Accept the existing key=value records, including embedded JSON objects."""
    result = {}
    cursor = 0
    while cursor < len(message):
        match = FIELD.match(message, cursor)
        if not match:
            break
        cursor = match.end()
        if cursor < len(message) and message[cursor] in "{[":
            value, length = DECODER.raw_decode(message[cursor:])
            cursor += length
        else:
            end = message.find(" ", cursor)
            if end == -1:
                end = len(message)
            raw = message[cursor:end]
            try:
                value = int(raw)
            except ValueError:
                value = raw
            cursor = end
        result[match[1]] = value
        while cursor < len(message) and message[cursor].isspace():
            cursor += 1
    return result


def parse_line(line: str) -> dict | None:
    match = TARGET.search(line.rstrip())
    if not match:
        return None
    target, message = match.groups()
    if target == "rtx-hdr" and not message.startswith("{"):
        return None
    payload = json.loads(message) if message.startswith("{") else parse_fields(message)
    if not isinstance(payload, dict) or not payload:
        raise ValueError("invalid diagnostic record")
    if target == "rtx-hdr":
        if payload.get("event") != "rtx_hdr_gpu_timing":
            return None
        for stage in ("gl_queue", "gl_sample", "cuda_pipeline"):
            payload[stage + "_timing_us"] = {"calls": payload["samples"],
                "total_us": payload[stage + "_us"], "max_us": payload[stage + "_max_us"]}
    # Older capture records have flat stage counters. These are interval totals.
    for name, count in list(payload.items()):
        if name.endswith("_calls"):
            stage = name.removesuffix("_calls")
            total, maximum = payload.get(stage + "_total_us"), payload.get(stage + "_max_us")
            if isinstance(total, int) and isinstance(maximum, int):
                payload[stage + "_us"] = {"calls": count, "total_us": total, "max_us": maximum}
    if target == "media.processing_latency":
        known = payload.get("known_samples", 0)
        if known:
            payload["capture_to_admission_us"] = {
                "calls": known, "total_us": payload["total_tenths_ms"] * 100,
                "max_us": payload["max_tenths_ms"] * 100,
            }
    return {"target": target, "timestamp": line[:match.start()].strip(), "payload": payload}


def metrics(value: dict, path: str = ""):
    if isinstance(value, list):
        for index, child in enumerate(value):
            if isinstance(child, dict):
                name = child.get("stage", str(index))
                yield from metrics(child, f"{path}.{name}")
        return
    if not isinstance(value, dict):
        return
    count = value.get("calls", value.get("packets"))
    if (isinstance(count, int) and count > 0
            and isinstance(value.get("total_us"), int) and isinstance(value.get("max_us"), int)
            and not isinstance(value.get("latency_us"), dict)):
        yield path, count, value
        return
    for key, child in value.items():
        if isinstance(child, (dict, list)):
            yield from metrics(child, f"{path}.{key}" if path else key)


def summarize(records: list[dict]) -> list[dict]:
    rows = {}
    expanded = []
    for record in records:
        if record["target"] == "broker.timing":
            for service in record["payload"].get("services", []):
                expanded.append({"target":record["target"], "payload":service})
        else:
            expanded.append(record)
    for record in expanded:
        payload = record["payload"]
        identity = next((f"{key}={payload[key]}" for key in
                         ("stream", "encoder_session", "module_type", "context_index", "principal") if key in payload), "provider")
        if record["target"] == "broker.timing":
            identity = f"caller={payload.get('caller')},service={payload.get('service')}"
        for path, count, metric in metrics(payload):
            key = record["target"], identity, path
            row = rows.setdefault(key, {"target": key[0], "identity": identity, "stage": path,
                                        "calls": 0, "total_us": 0, "max_us": 0,
                                        "max_observed_monotonic_us": None,
                                        "worst_window_p95_upper_us": None})
            # These metrics are reset at each report; never average percentiles.
            row["calls"] += count
            row["total_us"] += metric["total_us"]
            if metric["max_us"] >= row["max_us"]:
                row["max_us"] = metric["max_us"]
                row["max_observed_monotonic_us"] = metric.get("max_observed_monotonic_us")
            p95 = metric.get("p95_upper_us")
            if p95 is not None:
                row["worst_window_p95_upper_us"] = max(row["worst_window_p95_upper_us"] or 0, p95)
    for row in rows.values():
        row["mean_us"] = row["total_us"] / row["calls"]
    return sorted(rows.values(), key=lambda row: row["max_us"], reverse=True)


def coverage(records: list[dict]) -> dict:
    """Coverage is established only by actual records, never by a package name."""
    targets = {record["target"] for record in records}
    controls = [r["payload"] for r in records if r["target"] == "control.timing"]
    kernel = sum(d.get("timing_us", {}).get("kernel_residence_us", {}).get("calls", 0) for d in controls)
    gpu = sum(r["payload"].get("worker_metrics_cumulative", {}).get("native_stage_totals", {}).get("gpu_timed_samples", 0)
              for r in records if r["target"] == "encoder.timing")
    return {
        "module_host_calls": "observed" if "module.timing" in targets else "missing",
        "runtime_mutex_and_calls": "observed" if "runtime.invocation" in targets else "missing",
        "broker_queue_and_handler": "observed" if "broker.timing" in targets else "missing",
        "protocol_reordering_and_reassembly": "observed" if any("protocol" in d for d in controls) else "missing",
        "kernel_receive_residence": "observed" if kernel else "unavailable_or_missing",
        "audio_capture_worker_queue": "observed" if "audio.timing" in targets else "missing",
        "input_backend_calls": "observed" if "input.timing" in targets else "missing",
        "capture_calls": "observed" if "capture.presentation" in targets else "missing",
        "encoder_device_clock_stages": "observed" if gpu else "unavailable_or_missing",
        "video_transport_admission": "observed" if "media.processing_latency" in targets else "missing",
        "game_consumption_and_game_to_photon": "requires_game_or_external_instrumentation",
        "physical_network_delivery_and_display_scanout": "requires_peer_or_external_instrumentation",
        "os_scheduling_pressure_and_kernel_drop_counters": "observed" if "host.timing" in targets else "missing",
    }

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", type=Path, nargs="+", help="log files in chronological order")
    parser.add_argument("--windows", type=int, default=300, help="retain the last N diagnostic records (1..1000)")
    parser.add_argument("--limit", type=int, default=30, help="maximum timing rows to print")
    parser.add_argument("--json", action="store_true", help="include timing rows and original diagnostic windows")
    args = parser.parse_args()
    if not 1 <= args.windows <= 1000 or not 1 <= args.limit <= 1000:
        parser.error("windows and limit must be between 1 and 1000")
    records = deque(maxlen=args.windows)
    malformed = 0
    try:
        for path in args.logs:
            with path.open(encoding="utf-8-sig", errors="replace") as log:
                for line in log:
                    try:
                        record = parse_line(line)
                    except (ValueError, KeyError, TypeError):
                        malformed += 1
                        continue
                    if record:
                        records.append(record)
    except OSError as error:
        parser.exit(2, f"{error}\n")
    if not records:
        parser.exit(1, "No supported diagnostic windows found. Use logs from a build with streaming diagnostics enabled.\n")
    rows = summarize(list(records))
    if args.json:
        json.dump({"schema_version": 1, "malformed_records": malformed,
                   "coverage": coverage(list(records)), "metrics": rows, "records": list(records)}, sys.stdout, indent=2)
        print()
    else:
        print(f"Host diagnostics: {len(records)} records; {malformed} malformed records skipped.")
        print("Timings below overlap. Interval metrics describe cadence, not processing latency.")
        print("p95 is the worst window's histogram upper bound; it is not a combined percentile.")
        print("mean ms | p95 upper ms | max ms | calls | source / identity / stage")
        for row in rows[:args.limit]:
            p95 = row["worst_window_p95_upper_us"]
            tail = f"{p95 / 1000:.3f}" if p95 is not None else "unknown"
            print(f"{row['mean_us'] / 1000:7.3f} | {tail:>12} | {row['max_us'] / 1000:7.3f} | "
                  f"{row['calls']:5} | {row['target']} / {row['identity']} / {row['stage']}")
        missing = [name for name, state in coverage(list(records)).items()
                   if state in ("missing", "unavailable_or_missing")]
        if missing:
            print("Unmeasured or missing in these windows: " + ", ".join(missing))
        print("Inspect --json records for queue depths, oldest pending ages, deferrals and unknown samples.")
        print("Network transit, game response, client decoding and display latency require client-side evidence.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
