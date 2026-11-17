#!/usr/bin/env python3
"""Render analyst-curated scenarios from a raw stream failure audit.

Semantic identity/classification comes from a reviewable catalog, never from
counting syntax observations. Raw input is preserved, including unselected
evidence. Only Python's standard library is needed; no host/plugin is contacted.
"""
import argparse
from collections import Counter, defaultdict
import gzip
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys


CATEGORIES = {"critical_proposed", "recoverable_proposed", "unresolved", "expected_stop"}
LIMITS = [
    "This is a curated scenario catalog, not an exhaustive enumeration of runtime scenarios. Unselected raw observations remain an explicit backlog.",
    "Catalog IDs define semantic grouping. Multiple reasons/operations within one group are listed as variants; they are not asserted to be one runtime execution.",
    "Categories and priorities are analyst proposals, not measured criticality accuracy or authority to change runtime security/retirement behavior.",
    "AST and name-resolved caller chains are static candidates, not compiler-resolved data flow, runtime reachability, exception flow, or proof that errors escape catches/retries.",
    "A same-function central-policy reference does not prove dominance or final authority. Shared helper evidence can support several different scenarios without making them duplicates.",
    "Function excerpts supplement raw AST blind spots only after matching a recorded source-file SHA-256. An excerpt can be truncated; missing/changed evidence downgrades the category to unresolved.",
    "Missing providers, skipped symlinks, non-Rust/native/ABI, generated code, macros, dynamic plugins, selected package state, OS/hardware failures, queues/workers and IPC remain outside proven coverage.",
    "Fixture recall/false-positive results measure the labeled syntax/scenario fixtures only; they are not estimates of repository-wide scenario recall or runtime criticality accuracy.",
]

# Reviewed stream-owner APIs, not arbitrary methods called "fail" or "close".
# A definition must also have a retained path to a concrete state/response effect.
STREAM_SINKS = {
    ("amaterasu_stream_session", "Job", "fail"),
    ("amaterasu_rtsp", "Record", "close"),
    ("amaterasu_rtsp", "Record", "close_with_cause"),
    ("amaterasu_rtsp", "Controller", "fail_network_action"),
    ("amaterasu_rtsp", "Controller", "refuse_open"),
    ("amaterasu_nvhttp", "", "fail_record"),
    ("amaterasu_nvhttp", "", "fail_before_resume_stream"),
    ("amaterasu_nvhttp", "", "reject_resume"),
    ("amaterasu_nvhttp", "Record", "request_cleanup"),
    ("amaterasu_nvhttp", "Record", "request_transport_cleanup"),
    ("amaterasu_nvhttp", "Record", "request_monitored_transport_cleanup"),
    ("amaterasu_nvhttp", "Record", "fail_cleanup"),
}


def identity(fn):
    return (fn["package"], fn["owner"], fn["name"])


def local_effect_path(report, start):
    """A bounded same-package path to explicit sink effects, never an Err chain."""
    raw = report["raw"]
    functions = raw["inventory"]["functions"]
    observations = defaultdict(list)
    outgoing = defaultdict(list)
    for f in raw["findings"]:
        observations[f["function"]].append(f)
    for edge in raw["graph"]["edges"]:
        # Raw unique-name edges remain candidates. Only an annotated receiver
        # or same-namespace free function supports this local sink summary.
        if edge["confidence"] in ("annotated_receiver_candidate", "same_namespace_candidate"):
            outgoing[edge["caller"]].append(edge["callee"])
    queue = [(start, [start], [])]
    seen = set()
    while queue:
        current, path, hops = queue.pop(0)
        if current in seen:
            continue
        seen.add(current)
        for f in observations[current]:
            o = f["observation"]
            terminal_state = (o["kind"] == "lifecycle_transition"
                              and any(x in o["expression"] for x in ("State :: Stopping", "State :: Closing", "State :: CleanupFailed")))
            attempted_refusal = o["kind"] == "wire_status_candidate"
            if terminal_state or attempted_refusal:
                return {"functions": [functions[i]["id"] for i in path],
                        "causal_hops": hops,
                        "effect_finding_id": f["id"], "effect": o,
                        "confidence": "source_local_reviewed_stream_sink_path",
                        "limit": "Guards/callbacks/policy still condition execution; this is not runtime reachability or remote delivery."}
        if len(path) >= 5:
            continue
        for callee in outgoing[current]:
            if functions[callee]["package"] == functions[start]["package"]:
                edges = [edge for edge in raw["graph"]["edges"]
                         if edge["caller"] == current and edge["callee"] == callee
                         and edge["confidence"] in ("annotated_receiver_candidate", "same_namespace_candidate")]
                for edge in edges:
                    guarded = [f["observation"] for f in observations[current]
                               if f["observation"]["location"] == edge["location"]]
                    hop = dict(edge, caller_id=functions[current]["id"],
                               callee_id=functions[callee]["id"],
                               branch_evidence=guarded,
                               branch_limit="Only retained AST guards are known; no general interprocedural value-flow proof.")
                    queue.append((callee, path + [callee], hops + [hop]))
    return None


def discover(report):
    """Start at actual stream sinks, excluding generic Err/?, warnings and panics."""
    raw = report["raw"]
    functions = raw["inventory"]["functions"]
    by_position = defaultdict(list)
    for edge in raw["graph"]["edges"]:
        loc = edge["location"]
        by_position[(edge["caller"], loc["line"], loc["column"])].append(edge)
    sink_paths = {i: local_effect_path(report, i) for i, fn in enumerate(functions)
                  if identity(fn) in STREAM_SINKS}
    groups = {}
    mechanics = set(STREAM_SINKS) | {
        ("amaterasu_stream_session", "Job", "terminate"),
        ("amaterasu_stream_session", "Job", "stop_failed_or_cancelled"),
        ("amaterasu_rtsp", "Controller", "reply_status"),
    }
    for finding in raw["findings"]:
        o = finding["observation"]
        fn = functions[finding["function"]]
        if identity(fn) in mechanics:
            continue  # Shared sink machinery is support, not a new cause.
        if o["kind"] not in ("wire_status_candidate", "failure_boundary", "termination_candidate"):
            continue
        loc = o["location"]
        edges = by_position[(finding["function"], loc["line"], loc["column"])]
        sink_edges = [e for e in edges if identity(functions[e["callee"]]) in STREAM_SINKS]
        wire = (o["kind"] == "wire_status_candidate" and fn["package"] == "amaterasu_rtsp"
                and fn["owner"] == "Controller" and ". reply_status (" in o["expression"])
        if not wire and not sink_edges:
            continue
        status = None
        if wire:
            # New inventories retain argument positions and numeric values from
            # syn. Old three-argument inventories keep a conservative fallback;
            # never guess the numeric status from a later policy-report payload.
            status = o.get("literal_status")
            if status is None:
                match = re.search(r",\s*(\d+)\s*\)$", o["expression"])
                status = int(match.group(1)) if match else "dynamic_error_mapping"
            if isinstance(status, int) and not (400 <= status < 600):
                continue
        # Two AST nodes in the same causal branch (reply, close, revoke) are
        # one decision site. Different branch predicates stay separate.
        key = (fn["package"], fn["location"]["file"], fn["owner"], fn["name"], tuple(o["guards"]))
        group = groups.setdefault(key, {"function": fn["id"], "stable_function": list(key[:4]),
                                        "guards": o["guards"], "evidence_ids": [], "sites": [],
                                        "reason_codes": [], "wire_statuses": [], "local_causal_paths": [],
                                        "source": report["label"], "path_status": "uncertain_candidate"})
        group["evidence_ids"].append(finding["id"])
        group["sites"].append({"location": loc, "expression": o["expression"], "kind": o["kind"]})
        group["reason_codes"].extend(x for x in o["literals"] if re.fullmatch(r"[a-z][a-z0-9_]{2,95}", x))
        if wire:
            group["wire_statuses"].append(status)
            # Concrete negative status = an explicit source-level refusal.
            # Dynamic mappings require a separately curated causal review.
            if isinstance(status, int):
                group["path_status"] = "source_confirmed_rejection_site"
                group["effect_scope"] = "RTSP request refusal; stream survival is separate"
        for edge in sink_edges:
            path = sink_paths[edge["callee"]]
            if path:
                group["local_causal_paths"].append(dict(path, functions=[fn["id"]] + path["functions"]))
                if edge["confidence"] in ("annotated_receiver_candidate", "same_namespace_candidate"):
                    group["path_status"] = "source_confirmed_rejection_site"
                group["effect_scope"] = "stream/ticket failure or cleanup, conditional on the cited sink's guards and central directive"
        group.setdefault("effect_scope", "unresolved stream-owner sink invocation")
    result = []
    for key, group in groups.items():
        group["reason_codes"] = sorted(set(group["reason_codes"]))
        group["wire_statuses"] = sorted(set(group["wire_statuses"]), key=str)
        group["evidence_ids"] = sorted(set(group["evidence_ids"]))
        if not group["reason_codes"] and not group["wire_statuses"]:
            # A dynamic reason is not a distinct cause. Keep it in the uncertain
            # queue until an origin can be connected, rather than count it as one.
            group["path_status"] = "uncertain_candidate"
            group["origin_limit"] = "Dynamic reason/origin not established; not counted as a confirmed distinct cause."
        stable = json.dumps([list(key[:4]), list(key[4]), group["reason_codes"], group["wire_statuses"]], sort_keys=True)
        group["id"] = "sink-" + sha(stable.encode())[:12]
        group["category"] = "unresolved"
        group["classification_reason"] = "A real refusal/terminal instruction does not establish whether its underlying condition is critical or safely recoverable."
        group["caller_paths"] = paths_for(report, next(i for i, fn in enumerate(functions) if fn["id"] == group["function"]))
        result.append(group)
    return sorted(result, key=lambda g: (g["path_status"] != "source_confirmed_rejection_site", g["stable_function"], g["id"]))


def decision_groups(reports, scenarios):
    """Deduplicate explicit cause codes across sites/snapshots; keep variants."""
    generic = {"dependency_rejected", "dependency_unavailable", "negotiation_failed",
               "completion_unavailable", "response_unavailable", "invalid_request"}
    result = {}
    for report in reports:
        for site in discover(report):
            reason = site["reason_codes"]
            specific = bool(reason) and not any(code in generic for code in reason)
            key = ([site["stable_function"][0], reason, site["effect_scope"]] if specific
                   else [site["stable_function"], site["guards"], reason, site["wire_statuses"]])
            group_id = "cause-" + sha(json.dumps(key, sort_keys=True).encode())[:12]
            group = result.setdefault(group_id, {"id": group_id, "reason_codes": reason,
                                                 "variants": [], "category": "unresolved",
                                                 "curated_scenarios": []})
            group["variants"].append(site)
    for group in result.values():
        covered = []
        sites = {(v["source"], i) for v in group["variants"] for i in v["evidence_ids"]}
        for scenario in scenarios:
            anchors = {(e["report"], e["id"]) for e in scenario["evidence"]}
            triggers = {(e["report"], e["id"]) for e in scenario["evidence"] if set(e["roles"]) & {"trigger", "effect"}}
            if sites & triggers:
                group["curated_scenarios"].append(scenario["id"])
                if sites <= anchors:
                    covered.append(scenario["category"])
        if len(set(covered)) == 1:
            group["category"] = covered[0]
        confirmed = [v for v in group["variants"] if v["path_status"] == "source_confirmed_rejection_site"]
        group["path_status"] = ("source_confirmed_rejection_sites" if len(confirmed) == len(group["variants"])
                                else "mixed_confirmed_and_uncertain_variants" if confirmed else "uncertain_candidate")
    return sorted(result.values(), key=lambda g: (g["path_status"] == "uncertain_candidate", g["reason_codes"], g["id"]))


def sha(data):
    return hashlib.sha256(data).hexdigest()


def load_report(label, path):
    path = Path(path).resolve(strict=True)
    stored = path.read_bytes()
    payload = gzip.decompress(stored) if stored.startswith(b"\x1f\x8b") else stored
    raw = json.loads(payload)
    if raw.get("schema_version") != 1:
        raise ValueError(f"{label}: unsupported raw schema")
    functions = raw["inventory"]["functions"]
    if len({f["id"] for f in raw["findings"]}) != len(raw["findings"]):
        raise ValueError(f"{label}: duplicate raw finding IDs")
    incoming = defaultdict(list)
    for index, edge in enumerate(raw["graph"]["edges"]):
        if not (0 <= edge["caller"] < len(functions) and 0 <= edge["callee"] < len(functions)):
            raise ValueError(f"{label}: invalid graph indexes")
        incoming[edge["callee"]].append(index)
    return {"label": label, "path": str(path), "raw": raw,
            "stored_sha256": sha(stored), "raw_sha256": sha(payload),
            "incoming": incoming, "source_cache": {}}


def checked_source(report, file):
    """Accept current bytes or retained commit bytes only if the raw digest agrees."""
    if file in report["source_cache"]:
        return report["source_cache"][file]
    raw = report["raw"]
    manifest = next((f for f in raw["inventory"]["files"] if f["path"] == file), None)
    if not manifest or Path(file).is_absolute() or ".." in Path(file).parts:
        report.setdefault("source_failures", {})[file] = "No safe source path/digest in the recorded file manifest."
        return None
    root = Path(raw["source"].get("root", ".")).resolve()
    candidate = (root / file).resolve()
    data = None
    if candidate.is_relative_to(root):
        try:
            value = candidate.read_bytes()
            if sha(value) == manifest["sha256"]:
                data = value
        except OSError:
            pass
    head = raw["source"].get("git_head_before", "")
    if data is None and re.fullmatch(r"[a-fA-F0-9]{40,64}", head):
        # Read-only local Git fallback; no checkout, shell, network or plugin load.
        result = subprocess.run(["git", "--no-optional-locks", "-C", str(root),
                                 "show", f"{head}:{file}"], capture_output=True, check=False)
        if result.returncode == 0 and sha(result.stdout) == manifest["sha256"]:
            data = result.stdout
    answer = None if data is None else data.decode("utf-8")
    if answer is None:
        report.setdefault("source_failures", {})[file] = "Neither current nor recorded-commit bytes matched the scan SHA-256; source is changed or unavailable. Retained AST excerpts remain evidence."
    report["source_cache"][file] = answer
    return answer


def function_excerpt(report, index, selector):
    functions = report["raw"]["inventory"]["functions"]
    fn = functions[index]
    location = fn["location"]
    source = checked_source(report, location["file"])
    if source is None:
        return None
    lines = source.splitlines()
    start = location["line"]
    next_line = min((f["location"]["line"] for f in functions
                     if f["location"]["file"] == location["file"]
                     and f["location"]["line"] > start), default=len(lines) + 1)
    end = min(next_line - 1, start + 119)
    snippet = "\n".join(lines[start - 1:end])
    if any(value not in snippet for value in selector.get("contains", [])):
        return None
    if selector.get("contains_any") and not any(value in snippet for value in selector["contains_any"]):
        return None
    return {"id": "function:" + fn["id"], "function_index": index,
            "location": location, "evidence": snippet, "guards": [],
            "kind": "verified_function_excerpt", "literals": [],
            "excerpt_truncated": end < next_line - 1,
            "confidence": "file_sha256_verified_source_excerpt"}


def paths_for(report, function, finding=None):
    raw = report["raw"]
    if finding is not None:
        paths = finding["call_paths"]
    else:
        paths, stack = [], [([function], [])]
        while stack and len(paths) < 3:
            nodes, edges = stack.pop()
            next_edges = [i for i in report["incoming"].get(nodes[-1], [])
                          if raw["graph"]["edges"][i]["caller"] not in nodes]
            if not next_edges or len(edges) >= 5:
                paths.append({"functions": list(reversed(nodes)),
                              "edges": list(reversed(edges)), "bounded": bool(next_edges)})
            else:
                for edge in reversed(next_edges):
                    stack.append((nodes + [raw["graph"]["edges"][edge]["caller"]], edges + [edge]))
    return [{"functions": [raw["inventory"]["functions"][i]["id"] for i in p["functions"]],
             "edges": [raw["graph"]["edges"][i] for i in p["edges"]],
             "bounded": p["bounded"], "confidence": "candidate_static_name_resolution"}
            for p in paths]


def select(report, selector):
    if selector.get("reports") and report["label"] not in selector["reports"]:
        return []
    raw = report["raw"]
    functions = raw["inventory"]["functions"]

    def function_matches(fn):
        return (fn["location"]["file"] == selector["file"]
                and ("function" not in selector or fn["name"] == selector["function"])
                and ("function_any" not in selector or fn["name"] in selector["function_any"])
                and ("owner" not in selector or fn["owner"] == selector["owner"]))

    selected = []
    if selector.get("kind") == "function_excerpt":
        for index, fn in enumerate(functions):
            if function_matches(fn):
                evidence = function_excerpt(report, index, selector)
                if evidence:
                    evidence["call_paths"] = paths_for(report, index)
                    selected.append(evidence)
    else:
        for finding in raw["findings"]:
            o = finding["observation"]
            if not function_matches(functions[finding["function"]]):
                continue
            if "kind" in selector and selector["kind"] != o["kind"]:
                continue
            if any(value not in o["literals"] for value in selector.get("literals", [])):
                continue
            if any(value not in o["expression"] for value in selector.get("contains", [])):
                continue
            if any(not any(value in guard for guard in o["guards"]) for value in selector.get("guards", [])):
                continue
            evidence = dict(o, id=finding["id"], function_index=finding["function"],
                            confidence="retained_ast_observation_not_runtime_proof",
                            central_decision_review=finding["central_decision_review"],
                            paths_truncated=finding["paths_truncated"],
                            call_paths=paths_for(report, finding["function"], finding))
            selected.append(evidence)
    for evidence in selected:
        evidence["report"] = report["label"]
        evidence["function_id"] = functions[evidence["function_index"]]["id"]
        evidence["roles"] = [selector.get("role", "trigger")]
    return selected


def summarize(catalog, reports):
    if catalog.get("schema_version") != 1:
        raise ValueError("unsupported scenario catalog schema")
    ids = [s["id"] for s in catalog["scenarios"]]
    if len(set(ids)) != len(ids):
        raise ValueError("duplicate scenario IDs in catalog")
    scenarios, references, primary = [], set(), defaultdict(set)
    for item in catalog["scenarios"]:
        if item["category"] not in CATEGORIES or item["priority"] not in ("P0", "P1", "P2"):
            raise ValueError("invalid category/priority")
        dedup, missing = {}, []
        snapshot_status = {r["label"]: {"trigger_present": False, "missing_required_selectors": [], "missing_evidence_notes": []} for r in reports}
        for selector in item["selectors"]:
            hits = []
            for report in reports:
                selected = select(report, selector)
                hits.extend(selected)
                if selected and selector.get("role", "trigger") == "trigger":
                    snapshot_status[report["label"]]["trigger_present"] = True
                if not selected and not selector.get("optional", False):
                    snapshot_status[report["label"]]["missing_required_selectors"].append(selector)
                    note = (report.get("source_failures", {}).get(selector["file"], "No matching indexed function/excerpt in this snapshot.")
                            if selector.get("kind") == "function_excerpt" else "No matching AST observation in this snapshot; do not infer the old rejection still exists.")
                    snapshot_status[report["label"]]["missing_evidence_notes"].append(dict(file=selector["file"], reason=note))
            if not hits and not selector.get("optional", False):
                missing.append(selector)
            for hit in hits:
                key = (hit["report"], hit["id"])
                if key in dedup:
                    dedup[key]["roles"] = sorted(set(dedup[key]["roles"] + hit["roles"]))
                else:
                    dedup[key] = hit
                if not hit["id"].startswith("function:"):
                    references.add(key)
                if "trigger" in hit["roles"]:
                    primary[key].add(item["id"])
        evidence = sorted(dedup.values(), key=lambda e: (e["report"], e["location"]["file"], e["location"]["line"], e["id"]))
        has_trigger = any("trigger" in e["roles"] for e in evidence)
        category = item["category"] if has_trigger and not missing else "unresolved"
        for snapshot in snapshot_status.values():
            snapshot["category"] = (item["category"] if snapshot["trigger_present"] and not snapshot["missing_required_selectors"] else "unresolved")
            snapshot["status"] = ("required_anchors_present" if snapshot["trigger_present"] and not snapshot["missing_required_selectors"] else "not_observed_or_incomplete_evidence")
        scenarios.append(dict(item, selectors=None, category=category,
                              catalog_category=item["category"],
                              path_status=item.get("path_status", "uncertain_candidate") if has_trigger and not missing else "uncertain_candidate_missing_evidence",
                              confidence="static_anchors_present_not_runtime_proof" if has_trigger and not missing else "incomplete_static_evidence",
                              missing_required_selectors=missing, snapshot_status=snapshot_status, evidence=evidence))
    scenarios.sort(key=lambda s: (s["category"] == "expected_stop", s["priority"], s["id"]))
    total = sum(len(r["raw"]["findings"]) for r in reports)
    groups = decision_groups(reports, scenarios)
    return {"schema_version": 2, "grouping": "reviewed_semantic_catalog_plus_source_sink_anchored_decision_groups",
            "catalog_sha256": sha(json.dumps(catalog, sort_keys=True).encode()),
            "limitations": LIMITS,
            "summary": {"catalog_scenarios": len(scenarios),
                        "source_anchored_scenarios": sum(s["confidence"] != "incomplete_static_evidence" for s in scenarios),
                        "source_anchored_failure_or_recovery_scenarios": sum(s["confidence"] != "incomplete_static_evidence" and s["category"] != "expected_stop" for s in scenarios),
                        "failure_or_recovery_scenarios": sum(s["category"] != "expected_stop" for s in scenarios),
                        "expected_stop_scenarios": sum(s["category"] == "expected_stop" for s in scenarios),
                        "categories": dict(Counter(s["category"] for s in scenarios)),
                        "priorities": dict(Counter(s["priority"] for s in scenarios)),
                        "raw_observations": total,
                        "distinct_raw_observations_selected": len(references),
                        "raw_observations_not_selected": total - len(references),
                        "missing_evidence_scenarios": sum(s["confidence"] == "incomplete_static_evidence" for s in scenarios)},
            "primary_anchor_overlaps_for_review": [{"report": key[0], "evidence_id": key[1], "scenarios": sorted(values)} for key, values in sorted(primary.items()) if len(values) > 1],
            "sources": [{"label": r["label"], "path": r["path"], "raw_sha256": r["raw_sha256"], "stored_sha256": r["stored_sha256"], "source": r["raw"]["source"], "coverage_gaps": r["raw"]["inventory"]["gaps"], "errors": r["raw"]["inventory"]["errors"], "supplement_verification_gaps": r.get("source_failures", {})} for r in reports],
            "decision_group_summary": {"deduplicated_cause_groups": sum(g["category"] != "expected_stop" for g in groups),
                                       "expected_stop_groups": sum(g["category"] == "expected_stop" for g in groups),
                                       "source_confirmed_only": sum(g["path_status"] == "source_confirmed_rejection_sites" and g["category"] != "expected_stop" for g in groups),
                                       "mixed_variants": sum(g["path_status"] == "mixed_confirmed_and_uncertain_variants" and g["category"] != "expected_stop" for g in groups),
                                       "uncertain_only": sum(g["path_status"] == "uncertain_candidate" and g["category"] != "expected_stop" for g in groups)},
            "decision_groups": groups, "scenarios": scenarios}


def render(result):
    lines = ["# Prioritized stream failure review scenarios", "",
             "Categories are tentative. P0 means preserve/inspect a security or ownership boundary first; P1 means investigate recovery/admission behavior; P2 means resolve contract or propagation uncertainty.", "",
             f"{result['summary']['source_anchored_failure_or_recovery_scenarios']} source-anchored failure/recovery entries and {result['summary']['expected_stop_scenarios']} expected-stop entry. {result['summary']['missing_evidence_scenarios']} of {result['summary']['catalog_scenarios']} catalog entries have incomplete/unmatched evidence in these inputs. Categories remain proposed: {result['summary']['categories']}. These are review groups with explicit variants, not a claim to enumerate every runtime failure.", "",
             f"Collapsed {result['summary']['distinct_raw_observations_selected']} selected raw observations across the supplied snapshots. {result['summary']['raw_observations_not_selected']} raw observations remain outside this selected catalog; raw files are preserved.", "",
             "| Priority | Scenario | Tentative category | Path status | Effect scope |", "|---|---|---|---|---|"]
    for s in result["scenarios"]:
        lines.append(f"| {s['priority']} | {s['id']}: {s['title']} | {s['category']} | {s['path_status']} | {s['scope']} |")
    lines += ["", "## Source snapshots and coverage", ""]
    for source in result["sources"]:
        lines += [f"- `{source['label']}`: `{source['source'].get('git_head_before')}`; raw SHA-256 `{source['raw_sha256']}`."]
        lines += [f"  - GAP: {gap}" for gap in source["coverage_gaps"]]
        lines += [f"  - ERROR: {error}" for error in source["errors"]]
    lines += ["", "## Scenario evidence and next decisions", ""]
    for s in result["scenarios"]:
        lines += [f"### {s['id']} — {s['title']}", "",
                  f"**{s['priority']} · {s['category']} · {s['scope']}**", "",
                  f"Trigger: {s['trigger']}", f"Variants: {', '.join(s['variants'])}.", "",
                  f"{'Catalog behavior (not established for this input)' if s['missing_required_selectors'] else 'Source-backed behavior'}: {s['behavior']}", "",
                  f"Reason for tentative category: {s['reason']}", "",
                  f"Next decision: {s['next_action']}", "",
                  f"Path status: `{s['path_status']}`. Confidence: `{s['confidence']}`. Caller chains remain static candidates.", ""]
        lines += ["Snapshot coverage: " + "; ".join(f"`{label}`: {state['status']} ({state['category']})" for label, state in s["snapshot_status"].items()), ""]
        for label, state in s["snapshot_status"].items():
            lines += [f"- `{label}` missing evidence: `{note['file']}` — {note['reason']}" for note in state["missing_evidence_notes"]]
        lines += [""]
        if s["missing_required_selectors"]:
            lines += [f"Missing required anchors: `{json.dumps(s['missing_required_selectors'], sort_keys=True)}`. Category downgraded to unresolved.", ""]
        # Full deduplicated evidence remains in JSON. Give each snapshot at least
        # one primary anchor before shared helpers, and keep prose readable.
        show = []
        for report in sorted({e["report"] for e in s["evidence"]}):
            candidates = [e for e in s["evidence"] if e["report"] == report]
            candidates.sort(key=lambda e: ("trigger" not in e["roles"], e["location"]["line"]))
            show.extend(candidates[:2])
        for e in show:
            loc = e["location"]
            lines += [f"- `{e['report']}` `{loc['file']}:{loc['line']}` ({', '.join(e['roles'])}; `{e['id']}`; {e['confidence']})."]
            if e["guards"]:
                lines.append("  - Guards: " + "; ".join(f"`{g}`" for g in e["guards"][-2:]))
            if e["call_paths"]:
                p = e["call_paths"][0]
                lines.append("  - Candidate call chain: " + " → ".join(f"`{fn}`" for fn in p["functions"]) + (" (bounded)." if p["bounded"] else "."))
            lines += ["", "```rust", "\n".join(e["evidence"].splitlines()[:12]), "```", ""]
        lines += [f"{len(s['evidence'])} deduplicated evidence anchors in JSON; displayed excerpts are capped. No runtime effect or safe fallback is proved by a caller chain.", ""]
    lines += ["## Source-path index (cross-references, not additional scenario counts)", "",
              "These start at reviewed stream-owner sinks, not generic Err, ?, panic, warning or resource-close names. A confirmed site explicitly calls a known sink along a retained local path to a negative reply or stream state effect. Execution remains conditional on the cited guards and central directive; remote delivery is not proved.", "",
              f"Decision groups: `{result['decision_group_summary']}`. Repeated explicit cause codes are grouped across sites/snapshots with their branch variants retained. Generic/dynamic causes remain context-scoped candidates. Shared terminal helpers are not counted as new causes.", "",
              "The reviewed cases and this index overlap; do not add their counts. The case IDs identify reviewed semantic groups. Other entries remain unreviewed source-level cause groups with branch variants, not an exhaustive runtime enumeration.", "",
              "| Cause | Reviewed cases | Category | Snapshot/site | Source-local effect path |", "|---|---|---|---|---|"]
    for group in result["decision_groups"]:
        if group["path_status"] == "uncertain_candidate" or group["category"] == "expected_stop":
            continue
        site = next(v for v in group["variants"] if v["path_status"] == "source_confirmed_rejection_site")
        loc = site["sites"][0]["location"]
        path = " → ".join(site["local_causal_paths"][0]["functions"]) if site["local_causal_paths"] else "explicit negative RTSP reply"
        reason = ", ".join(group["reason_codes"]) or ", ".join(str(c) for c in site["wire_statuses"])
        lines.append(f"| `{reason}` ({len(group['variants'])} variants) | {', '.join(group['curated_scenarios']) or 'unreviewed'} | {group['category']} | `{site['source']}:{loc['file']}:{loc['line']}` | `{path}` |")
    lines += ["", "Uncertain candidates are kept separately in JSON `decision_groups` with `path_status=uncertain_candidate`, including unresolved reason origins and untyped dispatch. They are not counted as confirmed rejection causes. Mixed groups retain per-variant confidence. This selected sink registry does not cover all lifecycle/provider/network boundaries.", "",
              "## Analysis limits", ""] + [f"- {limit}" for limit in result["limitations"]]
    if result["primary_anchor_overlaps_for_review"]:
        lines += ["", "Primary anchor overlap requires semantic review:", json.dumps(result["primary_anchor_overlaps_for_review"], indent=2)]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", action="append", required=True, metavar="LABEL=RAW.json[.gz]")
    parser.add_argument("--catalog", type=Path, default=Path(__file__).parent / "stream-failure-audit" / "scenarios.json")
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--markdown", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        reports = []
        for item in args.report:
            label, path = item.split("=", 1)
            if not re.fullmatch(r"[a-zA-Z0-9_-]+", label) or label in {r['label'] for r in reports}:
                raise ValueError("invalid/duplicate report label")
            reports.append(load_report(label, path))
        outputs = [args.json, args.markdown]
        input_paths = {Path(r["path"]) for r in reports} | {args.catalog.resolve()}
        for output in outputs:
            if output.is_symlink() or output.resolve() in input_paths:
                raise ValueError("refusing to overwrite a raw input/catalog or symlink")
            for report in reports:
                root = Path(report["raw"]["source"].get("root", ".")).resolve()
                if any(output.resolve().is_relative_to(root / p) for p in ("crates", "modules", ".git")):
                    raise ValueError("refusing output inside source inputs")
        if outputs[0].resolve() == outputs[1].resolve():
            raise ValueError("JSON and Markdown outputs must differ")
        result = summarize(json.loads(args.catalog.read_text(encoding="utf-8")), reports)
        args.json.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
        args.markdown.write_text(render(result), encoding="utf-8")
        print(json.dumps(result["summary"], sort_keys=True))
        return 0
    except (OSError, ValueError, KeyError, IndexError, TypeError, UnicodeError, EOFError) as error:
        print(f"summarize-stream-failures: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
