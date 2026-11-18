#!/usr/bin/env python3
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from streaming_diagnostics import parse_line, summarize, coverage, slow_moments


def capture(target, payload):
    return parse_line(json.dumps({"target": target, "source": "test", "timestamp": 0, "payload": payload}))


class StreamingDiagnosticsTests(unittest.TestCase):
    def test_flow_trace_windows_name_their_track_and_slow_flows_say_why(self):
        step = {"stage": "socket.arrived>control.authenticated", "calls": 900,
                "total_us": 684000, "max_us": 19810, "p95_upper_us": 2000}
        window = capture("trace.window", {
            "event": "trace_window", "window_us": 10000000,
            "tracks": [{"track": "input", "completed": 900, "slow": 1,
                        "total": {"stage": "total", "calls": 900, "total_us": 882000, "max_us": 23412},
                        "steps": [step]}],
            "spans": [{"stage": "host.tick:amaterasu.state", "calls": 1, "total_us": 21438, "max_us": 21438}]})
        slow = capture("trace.slow_flow", {
            "event": "slow_flow", "track": "input", "total_us": 23412, "budget_us": 8000,
            "slowest_step": {"from": "socket.arrived", "to": "control.authenticated", "us": 19810, "percent": 84,
                             "during": [{"span": "host.tick", "label": "amaterasu.state", "us": 21438}]}})
        span = capture("trace.slow_span", {
            "event": "slow_span", "span": "host.tick", "label": "amaterasu.state", "us": 21438})
        rows = {row["stage"]: row for row in summarize([window])}
        self.assertEqual(rows["tracks.input.steps.socket.arrived>control.authenticated"]["max_us"], 19810)
        self.assertEqual(rows["tracks.input.total"]["calls"], 900)
        self.assertEqual(rows["spans.host.tick:amaterasu.state"]["max_us"], 21438)
        self.assertEqual(coverage([window])["cross_module_flow_steps"], "observed")
        self.assertEqual(coverage([span])["cross_module_flow_steps"], "missing")
        self.assertEqual(slow_moments([window, slow, span]), [
            "1970-01-01 00:00:00 UTC input flow took 23.4 ms (budget 8.0 ms); slowest step "
            "socket.arrived>control.authenticated 19.8 ms (84%) during host.tick amaterasu.state 21.4 ms",
            "1970-01-01 00:00:00 UTC span host.tick amaterasu.state took 21.4 ms",
        ])

    def test_dedicated_worker_cost_is_separate_from_host_loop_cost(self):
        records = [capture(target, {
            "module_type": "example::Service", "tick_us": {
                "calls": 1, "total_us": value, "max_us": value}})
            for target, value in (("service.worker", 17000), ("module.timing", 20))]
        rows = summarize(records)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["target"], "service.worker")
        self.assertEqual(rows[0]["max_us"], 17000)
        self.assertEqual(rows[1]["max_us"], 20)

    def test_zero_and_missing_are_not_combined_and_streams_stay_separate(self):
        def record(stream, metric):
            return capture("video.pipeline", {
                "stream": stream, "stage_timing_us": {"capture_callback": metric}})
        records = [record(1, {"calls": 0, "total_us": 0, "max_us": 0}),
                   record(1, {"calls": 1, "total_us": 0, "max_us": 0, "p95_upper_us": 0}),
                   record(2, {"calls": 1, "total_us": 8000, "max_us": 8000})]
        rows = summarize(records)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["identity"], "stream=2")
        self.assertIsNone(rows[0]["worst_window_p95_upper_us"])
        self.assertEqual(rows[1]["worst_window_p95_upper_us"], 0)

    def test_replacement_providers_with_the_same_local_stream_are_distinct(self):
        payload = {"stream": 7, "timing": {"calls": 1, "total_us": 1000, "max_us": 1000}}
        original = capture("video.pipeline", payload)
        replacement = {**original, "source": "replacement"}
        rows = summarize([original, replacement])
        self.assertEqual({row["source"] for row in rows}, {"test", "replacement"})
        self.assertTrue(all(row["calls"] == 1 for row in rows))

    def test_existing_capture_and_wire_units_are_converted_explicitly(self):
        sample = capture("capture.presentation", {"wait_calls": 2, "wait_total_us": 6000, "wait_max_us": 4000})
        media = capture("media.processing_latency", {"known_samples": 2, "total_tenths_ms": 40, "max_tenths_ms": 30})
        rows = summarize([sample, media])
        self.assertEqual(rows[0]["mean_us"], 3000)
        self.assertEqual(rows[1]["mean_us"], 2000)
        self.assertEqual(rows[1]["max_us"], 3000)

    def test_cli_reports_malformed_records_without_losing_valid_windows(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "amaterasu.log"
            log.write_text('{broken\n' +
                           json.dumps({"target": "input.timing", "timestamp": 0, "source": "test", "payload": {"context_index": 1, "timing_us": {
                               "event_injection_us": {"calls": 1, "total_us": 2000, "max_us": 2000}}}}),
                           encoding="utf-8")
            output = subprocess.check_output([sys.executable,
                str(Path(__file__).with_name("streaming_diagnostics.py")), str(log), "--json"], text=True)
            report = json.loads(output)
            self.assertEqual(report["malformed_records"], 1)
            self.assertEqual(len(report["records"]), 1)
            self.assertEqual(report["metrics"][0]["max_us"], 2000)

    def test_rtx_hdr_gpu_stages_are_reported_without_counting_startup_messages(self):
        payload = {"event": "rtx_hdr_gpu_timing", "samples": 2,
                   "gl_queue_us": 100, "gl_queue_max_us": 80,
                   "gl_sample_us": 500, "gl_sample_max_us": 300,
                   "cuda_pipeline_us": 8000, "cuda_pipeline_max_us": 5000}
        rows = summarize([capture("rtx-hdr", payload)])
        self.assertEqual(len(rows), 3)
        self.assertEqual(rows[0]["mean_us"], 4000)
        self.assertEqual(rows[0]["max_us"], 5000)

    def test_broker_rows_keep_service_identity_across_row_reordering(self):
        metric={"calls":1,"total_us":5000,"max_us":5000,"max_observed_monotonic_us":1234}
        services=[{"caller":"owner","service":name,"timing_us":{"queue_us":metric}} for name in ("a/1","b/1")]
        first=capture("broker.timing", {"services":services})
        second=capture("broker.timing", {"services":list(reversed(services))})
        rows=summarize([first,second])
        self.assertEqual(len(rows),2)
        self.assertTrue(all(row['calls']==2 for row in rows))
        self.assertTrue(all(row['max_observed_monotonic_us']==1234 for row in rows))
    def test_missing_boundary_is_never_reported_as_measured_zero(self):
        result=coverage([capture("input.timing", {"context_index":0})])
        self.assertEqual(result['input_backend_calls'],'observed')
        self.assertEqual(result['protocol_reordering_and_reassembly'],'missing')
        self.assertEqual(result['kernel_receive_residence'],'unavailable_or_missing')
    def test_named_storage_stages_are_not_combined_by_array_position(self):
        metric={"calls":1,"total_us":20000,"max_us":20000}
        row=capture("module.timing", {"module_type":"state","stages":[{"stage":"storage.file_sync","timing_us":metric}]})
        self.assertEqual(summarize([row])[0]['stage'],'stages.storage.file_sync.timing_us')


if __name__ == "__main__":
    unittest.main()
