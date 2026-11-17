#!/usr/bin/env python3
"""Scenario regression tests, including a real checkout end-to-end audit."""
from copy import deepcopy
import gzip
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import audit_stream_failures as launcher

import summarize_stream_failures as audit


def synthetic_report(label="source"):
    file = "modules/stream-session/src/pipeline/workflow.rs"
    def function(name, owner="", package="amaterasu_stream_session"):
        return dict(id=f"{file}:{name}:{owner}", name=name, owner=owner, package=package,
                    namespace="pipeline::workflow", location=dict(file=file, line=1, column=1))
    functions = [function("capture_failed"), function("fail", "Job"),
                 function("optional_failed"), function("local_file_close", package="other"),
                 function("close", "File", package="other")]
    def finding(id, index, kind, expression, code, line, guards=()):
        return dict(id=id, function=index, classification="unresolved", relevance="stream_scope",
                    central_decision_review="unconfigured", paths_truncated=False,
                    call_paths=[dict(functions=[index], edges=[], bounded=False)],
                    observation=dict(kind=kind, expression=expression, evidence=expression,
                                     guards=list(guards), literals=[code] if code else [],
                                     location=dict(file=file, line=line, column=1)))
    findings = [
        finding("cause-a", 0, "failure_boundary", 'job . fail (record , "capture_unavailable" , now)', "capture_unavailable", 10, ["if missing"]),
        finding("cause-b", 0, "failure_boundary", 'job . fail (record , "capture_unavailable" , now)', "capture_unavailable", 20, ["if disconnected"]),
        finding("effect", 1, "lifecycle_transition", "record . state = State :: Stopping", None, 30),
        finding("optional-origin", 2, "error_origin", 'Err ("optional_history_unavailable")', "optional_history_unavailable", 40),
        finding("optional-warning", 2, "declared_disposition", 'StreamIssue :: demoted ("history_unavailable")', "history_unavailable", 41),
        finding("file-close", 3, "termination_candidate", "file . close ()", None, 50),
    ]
    edges = [dict(caller=0, callee=1, confidence="annotated_receiver_candidate", location=dict(file=file, line=line, column=1)) for line in (10, 20)]
    edges += [dict(caller=3, callee=4, confidence="annotated_receiver_candidate", location=dict(file=file, line=50, column=1))]
    raw = dict(schema_version=1, source=dict(root="/fixture-unavailable", git_head_before="not-git"),
               inventory=dict(functions=functions, files=[], gaps=[], errors=[]), findings=findings,
               graph=dict(edges=edges))
    incoming = {1: [0, 1], 4: [2]}
    return dict(label=label, raw=raw, incoming=incoming, source_cache={}, path="fixture",
                raw_sha256="fixture", stored_sha256="fixture")


class ScenarioTests(unittest.TestCase):
    def test_new_wire_policy_argument_does_not_hide_the_status(self):
        report = synthetic_report()
        fn = report["raw"]["inventory"]["functions"][0]
        fn.update(package="amaterasu_rtsp", owner="Controller", name="handle_network")
        observation = report["raw"]["findings"][0]["observation"]
        observation.update(kind="wire_status_candidate", literal_status=455,
                           arguments=["1", "2", "455u16", 'StreamFailure::advisory("request_wrong_state")'],
                           expression='self . reply_status (1 , 2 , 455u16 , StreamFailure :: advisory ("request_wrong_state"))',
                           literals=["request_wrong_state"])
        report["raw"]["findings"] = [report["raw"]["findings"][0]]
        report["raw"]["graph"]["edges"] = []
        report["incoming"] = {}
        groups = audit.discover(report)
        self.assertEqual(groups[0]["wire_statuses"], [455])
        self.assertEqual(groups[0]["path_status"], "source_confirmed_rejection_site")
        self.assertIn("request refusal", groups[0]["effect_scope"])

    def test_known_sink_paths_and_semantic_dedup(self):
        report = synthetic_report()
        groups = audit.decision_groups([report, synthetic_report("other-snapshot")], [])
        self.assertEqual(len(groups), 1)
        group = groups[0]
        self.assertEqual(group["reason_codes"], ["capture_unavailable"])
        self.assertEqual(len(group["variants"]), 4)
        self.assertEqual(group["path_status"], "source_confirmed_rejection_sites")
        self.assertTrue(all(v["local_causal_paths"][0]["effect_finding_id"] == "effect" for v in group["variants"]))
        self.assertEqual(group["category"], "unresolved")

    def test_error_warning_and_file_close_are_not_rejection_scenarios(self):
        groups = audit.discover(synthetic_report())
        observed = {id for g in groups for id in g["evidence_ids"]}
        negatives = {"optional-origin", "optional-warning", "file-close"}
        self.assertFalse(observed & negatives)
        print("Scenario negative controls: 0/3 false positives (generic Err, optional warning, non-stream close)")

    def test_dynamic_cause_and_untyped_dispatch_remain_candidates(self):
        report = synthetic_report()
        for finding in report["raw"]["findings"][:2]:
            finding["observation"]["literals"] = []
        self.assertTrue(all(g["path_status"] == "uncertain_candidate" for g in audit.discover(report)))
        report = synthetic_report()
        for edge in report["raw"]["graph"]["edges"]:
            edge["confidence"] = "unique_name_candidate"
        self.assertTrue(all(g["path_status"] == "uncertain_candidate" for g in audit.discover(report)))

    def test_missing_anchor_downgrades_and_duplicate_ids_fail(self):
        scenario = dict(id="S1", title="missing", priority="P0", category="critical_proposed",
                        path_status="source_confirmed", selectors=[dict(file="absent.rs", role="trigger")])
        catalog = dict(schema_version=1, scenarios=[scenario])
        result = audit.summarize(catalog, [synthetic_report()])
        self.assertEqual(result["scenarios"][0]["category"], "unresolved")
        self.assertEqual(result["scenarios"][0]["path_status"], "uncertain_candidate_missing_evidence")
        catalog["scenarios"].append(deepcopy(scenario))
        with self.assertRaises(ValueError):
            audit.summarize(catalog, [synthetic_report()])

    def test_function_evidence_requires_the_recorded_source_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "modules/rtsp/src/lib.rs"
            path.parent.mkdir(parents=True)
            data = b"fn check() { reject(); }\n"
            path.write_bytes(data)
            report = synthetic_report()
            report["raw"]["source"]["root"] = str(root)
            report["raw"]["inventory"]["files"] = [dict(path="modules/rtsp/src/lib.rs", sha256=audit.sha(data))]
            self.assertEqual(audit.checked_source(report, "modules/rtsp/src/lib.rs"), data.decode())
            path.write_text("fn check() { allow(); }\n")
            report["source_cache"].clear()
            self.assertIsNone(audit.checked_source(report, "modules/rtsp/src/lib.rs"))

    def test_historical_case_does_not_imply_current_rejection(self):
        before = synthetic_report("before")
        after = synthetic_report("after")
        after["raw"]["findings"] = []
        case = dict(id="S1", title="capture", priority="P0", category="critical_proposed",
                    path_status="source_confirmed", selectors=[dict(file="modules/stream-session/src/pipeline/workflow.rs", kind="failure_boundary", literals=["capture_unavailable"], role="trigger")])
        result = audit.summarize(dict(schema_version=1, scenarios=[case]), [before, after])
        statuses = result["scenarios"][0]["snapshot_status"]
        self.assertEqual(statuses["before"]["category"], "critical_proposed")
        self.assertEqual(statuses["after"]["category"], "unresolved")
        self.assertEqual(statuses["after"]["status"], "not_observed_or_incomplete_evidence")

    def test_cli_preserves_raw_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root / "raw.json.gz"
            raw.write_bytes(gzip.compress(json.dumps(synthetic_report()["raw"]).encode(), mtime=0))
            before = raw.read_bytes()
            catalog = root / "catalog.json"
            catalog.write_text('{"schema_version":1,"scenarios":[]}')
            code = audit.main(["--report", f"source={raw}", "--catalog", str(catalog),
                               "--json", str(root / "scenario.json"), "--markdown", str(root / "scenario.md")])
            self.assertEqual(code, 0)
            self.assertEqual(raw.read_bytes(), before)
            code = audit.main(["--report", f"source={raw}", "--catalog", str(catalog),
                               "--json", str(raw), "--markdown", str(root / "scenario.md")])
            self.assertEqual(code, 2)
            self.assertEqual(raw.read_bytes(), before)

    def test_output_aliases_cannot_corrupt_inputs_or_each_other(self):
        for target in ("raw", "catalog", "markdown"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                raw = root / "raw.json.gz"
                raw.write_bytes(gzip.compress(json.dumps(synthetic_report()["raw"]).encode(), mtime=0))
                catalog = root / "catalog.json"
                catalog.write_text('{"schema_version":1,"scenarios":[]}')
                markdown = root / "scenario.md"
                markdown.write_text("existing report")
                source = {"raw": raw, "catalog": catalog, "markdown": markdown}[target]
                output = root / "scenario.json"
                try:
                    os.link(source, output)
                except OSError as error:
                    self.skipTest(f"hard links unavailable: {error}")
                before = {path: path.read_bytes() for path in (raw, catalog, markdown)}
                code = audit.main(["--report", f"source={raw}", "--catalog", str(catalog),
                                   "--json", str(output), "--markdown", str(markdown)])
                self.assertEqual(code, 2)
                for path, data in before.items():
                    self.assertEqual(path.read_bytes(), data)

    def test_launcher_rejects_aliases_before_running_the_analyzer(self):
        for target in ("catalog", "markdown"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                catalog = root / "catalog.json"
                catalog.write_text('{"schema_version":1,"scenarios":[]}')
                markdown = root / "report.md"
                markdown.write_text("existing report")
                raw = root / "report.raw.json.gz"
                source = catalog if target == "catalog" else markdown
                try:
                    os.link(source, raw)
                except OSError as error:
                    self.skipTest(f"hard links unavailable: {error}")
                before = source.read_bytes()
                with mock.patch.object(launcher.subprocess, "run") as run:
                    code = launcher.main(["--root", str(root), "--catalog", str(catalog),
                        "--json", str(root / "report.json"), "--raw-json", str(raw), "--markdown", str(markdown)])
                    run.assert_not_called()
                self.assertEqual(code, 2)
                self.assertEqual(source.read_bytes(), before)

    def test_artifact_replace_preserves_a_hard_linked_source_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_bytes(b"preserve this input")
            output = root / "report.json"
            try:
                os.link(source, output)
            except OSError as error:
                self.skipTest(f"hard links unavailable: {error}")
            audit.write_artifact(output, b'{"new":"artifact"}')
            self.assertEqual(source.read_bytes(), b"preserve this input")
            self.assertEqual(json.loads(output.read_text()), {"new":"artifact"})

    def test_real_checkout_default_output_follows_actual_stream_sinks(self):
        # This invokes the actual Rust analyzer against the current repository,
        # not a toy source fixture or a hand-written imitation of its output.
        tool_repository = Path(__file__).resolve().parents[1]
        repository = Path(os.environ.get("AMATERASU_AUDIT_TEST_ROOT", tool_repository)).resolve()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = [sys.executable, str(tool_repository / "scripts/audit_stream_failures.py"),
                       "--root", str(repository), "--json", str(root / "scenarios.json"),
                       "--markdown", str(root / "scenarios.md")]
            result = subprocess.run(command, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            report = json.loads((root / "scenarios.json").read_text())
            self.assertEqual(report["schema_version"], 2)
            confirmed = [g for g in report["decision_groups"] if g["path_status"] != "uncertain_candidate"]
            codes = {code for g in confirmed for code in g["reason_codes"]}
            positive_codes = {"streaming_disabled", "lifecycle_journal_write_failed", "capture_binding_invalid"}
            self.assertTrue(positive_codes <= codes, f"missing real rejection causes: {positive_codes-codes}")
            backlog_refusal = any(503 in v["wire_statuses"] and v["path_status"] == "source_confirmed_rejection_site"
                                  and "Controller::poll_transport" in v["function"]
                                  for g in confirmed for v in g["variants"])
            network_source = (repository / "modules/rtsp/src/controller/network.rs").read_text(encoding="utf-8")
            if "poll_with_request_budget" in network_source:
                self.assertFalse(backlog_refusal, "removed backlog 503 must not remain a confirmed rejection")
                backlog_behavior = "budget/backpressure; no stale backlog refusal"
            else:
                self.assertTrue(backlog_refusal)
                backlog_behavior = "backlog 503 source refusal"
            # These actual repository error/warning origins are not direct
            # confirmed stream rejections. They remain candidate/catalog evidence.
            self.assertNotIn("invalid_processing_composition", codes)
            self.assertNotIn("frame_limiter_unavailable", codes)
            teardown = next(s for s in report["scenarios"] if s["id"] == "S24")
            self.assertEqual(teardown["category"], "expected_stop")
            teardown_ids = {(e["report"], e["id"]) for e in teardown["evidence"]}
            for group in report["decision_groups"]:
                sites = {(v["source"], id) for v in group["variants"] for id in v["evidence_ids"]}
                if sites & teardown_ids:
                    self.assertEqual(group["category"], "expected_stop")
            self.assertEqual(report["decision_group_summary"]["deduplicated_cause_groups"],
                             len(report["decision_groups"]) - report["decision_group_summary"]["expected_stop_groups"])
            for g in confirmed:
                self.assertTrue(any(v["local_causal_paths"] or any(isinstance(s, int) and 400 <= s < 600 for s in v["wire_statuses"])
                                    for v in g["variants"] if v["path_status"] == "source_confirmed_rejection_site"))
            raw = root / "scenarios.raw.json.gz"
            self.assertTrue(raw.exists())
            inventory = json.loads(gzip.decompress(raw.read_bytes()))
            self.assertGreater(len(inventory["findings"]), len(report["decision_groups"]))
            print(f"Actual checkout examples: 3/3 required-failure causes found plus {backlog_behavior}; expected teardown excluded; 0/2 generic/optional origins promoted to confirmed rejection; "
                  + json.dumps(report["decision_group_summary"], sort_keys=True))

    def test_partial_source_reports_exact_error_and_retains_evidence(self):
        repository = Path(__file__).resolve().parents[1]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates/core/src").mkdir(parents=True)
            broken = root / "modules/rtsp/src/lib.rs"
            broken.parent.mkdir(parents=True)
            broken.write_text("fn incomplete(", encoding="utf-8")
            result = subprocess.run([sys.executable, str(repository / "scripts/audit_stream_failures.py"),
                                     "--root", str(root), "--json", str(root / "out.json")],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertIn("modules/rtsp/src/lib.rs", result.stderr)
            self.assertTrue((root / "out.raw.json.gz").exists())
            scenario_report = json.loads((root / "out.json").read_text())
            self.assertEqual(len(scenario_report["sources"][0]["errors"]), 1)
            self.assertEqual(scenario_report["decision_group_summary"]["source_confirmed_only"], 0)


if __name__ == "__main__":
    unittest.main()
