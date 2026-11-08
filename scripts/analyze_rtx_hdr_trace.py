#!/usr/bin/env python3
"""Summarize RTX HDR CPU API ordering in an existing ftrace uprobe capture.

This does not measure GPU execution or prove that a wait delayed a frame.
Capture map, unmap and NvVFX_Run entry/return events on the same thread.
"""
import argparse
import json
import re
import statistics

EVENT = re.compile(
    r"^\s*(?P<thread>\S+-\d+)\s+\[\d+\].*?\s"
    r"(?P<time>\d+\.\d+):\s+"
    r"(?P<name>cuGraphicsMapResources|cuGraphicsUnmapResources|NvVFX_Run)"
    r"_(?P<phase>begin|end):"
)


def analyze(lines):
    threads = {}
    result = {"model_calls_mapped": 0, "model_calls_after_unmap": 0,
              "model_calls_unknown": 0, "incomplete_calls": 0,
              "map_to_unmap_cpu_us": [], "model_call_cpu_us": []}
    for line in lines:
        match = EVENT.match(line)
        if not match:
            continue
        thread = threads.setdefault(match["thread"], {"active": {}, "mapped": None,
                                                        "unmapped": False})
        name, phase = match["name"], match["phase"]
        now = float(match["time"])
        if phase == "begin":
            if name in thread["active"]:
                result["incomplete_calls"] += 1
            thread["active"][name] = now
            if name == "NvVFX_Run":
                key = ("model_calls_mapped" if thread["mapped"] is not None else
                       "model_calls_after_unmap" if thread["unmapped"] else
                       "model_calls_unknown")
                result[key] += 1
            continue
        start = thread["active"].pop(name, None)
        if start is None or now < start:
            result["incomplete_calls"] += 1
            continue
        if name == "cuGraphicsMapResources":
            thread["mapped"] = now
            thread["unmapped"] = False
        elif name == "cuGraphicsUnmapResources":
            if thread["mapped"] is None:
                result["incomplete_calls"] += 1
            else:
                result["map_to_unmap_cpu_us"].append(round((now - thread["mapped"]) * 1e6, 1))
            thread["mapped"] = None
            thread["unmapped"] = True
        else:
            result["model_call_cpu_us"].append(round((now - start) * 1e6, 1))
    result["incomplete_calls"] += sum(len(t["active"]) for t in threads.values())
    for key in ("map_to_unmap_cpu_us", "model_call_cpu_us"):
        values = result[key]
        result[key] = {"samples": len(values),
                       "median": round(statistics.median(values), 1) if values else None,
                       "max": max(values) if values else None}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", help="ftrace text containing RTX HDR uprobes")
    args = parser.parse_args()
    with open(args.trace, encoding="utf-8") as trace:
        result = analyze(trace)
    print(json.dumps(result, indent=2))
    if not sum(result[key] for key in ("model_calls_mapped", "model_calls_after_unmap",
                                       "model_calls_unknown")):
        parser.exit(1, "No TrueHDR calls found; check trace event names and capture.\n")


if __name__ == "__main__":
    main()
