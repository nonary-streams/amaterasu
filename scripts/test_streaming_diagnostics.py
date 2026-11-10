#!/usr/bin/env python3
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from streaming_diagnostics import parse_line, summarize, coverage


class StreamingDiagnosticsTests(unittest.TestCase):
    def test_dedicated_worker_cost_is_separate_from_host_loop_cost(self):
        records = [parse_line('Info: example::' + target + ': ' + json.dumps({
            "module_type": "example::Service", "tick_us": {
                "calls": 1, "total_us": value, "max_us": value}}))
            for target, value in (("service.worker", 17000), ("module.timing", 20))]
        rows = summarize(records)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["target"], "service.worker")
        self.assertEqual(rows[0]["max_us"], 17000)
        self.assertEqual(rows[1]["max_us"], 20)

    def test_nested_legacy_records_keep_percentiles_and_do_not_double_count(self):
        metric = {"calls": 2, "total_us": 4000, "max_us": 3000, "p95_upper_us": 3000}
        line = ('[2026-11-08 10:00:00]: Info: amaterasu.encode::encoder.timing: '
                'encoder_session=7 elapsed_ms=10000 delivery_wait=' +
                json.dumps({"packets": 2, "total_us": 4000, "max_us": 3000, "latency_us": metric}) +
                ' worker_metrics_cumulative=' + json.dumps({"completed_operations": 900,
                "operation_us_total": 99000, "latency_window": {"submit_us": metric}}))
        record = parse_line(line)
        self.assertEqual(record["payload"]["encoder_session"], 7)
        rows = summarize([record])
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(row["calls"] == 2 for row in rows))
        self.assertTrue(all(row["worst_window_p95_upper_us"] == 3000 for row in rows))

    def test_zero_and_missing_are_not_combined_and_streams_stay_separate(self):
        def record(stream, metric):
            return parse_line('Info: video.pipeline: ' + json.dumps({
                "stream": stream, "stage_timing_us": {"capture_callback": metric}}))
        records = [record(1, {"calls": 0, "total_us": 0, "max_us": 0}),
                   record(1, {"calls": 1, "total_us": 0, "max_us": 0, "p95_upper_us": 0}),
                   record(2, {"calls": 1, "total_us": 8000, "max_us": 8000})]
        rows = summarize(records)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["identity"], "stream=2")
        self.assertIsNone(rows[0]["worst_window_p95_upper_us"])
        self.assertEqual(rows[1]["worst_window_p95_upper_us"], 0)

    def test_existing_capture_and_wire_units_are_converted_explicitly(self):
        capture = parse_line('Info: capture.presentation: stream=3 wait_calls=2 wait_total_us=6000 wait_max_us=4000')
        media = parse_line('Info: media.processing_latency: stream=4 known_samples=2 total_tenths_ms=40 max_tenths_ms=30 histogram=<=2.0ms:1,>2.0ms:1')
        rows = summarize([capture, media])
        self.assertEqual(rows[0]["mean_us"], 3000)
        self.assertEqual(rows[1]["mean_us"], 2000)
        self.assertEqual(rows[1]["max_us"], 3000)
        self.assertIsNone(parse_line('Info: unrelated: private user data'))

    def test_cli_reports_malformed_records_without_losing_valid_windows(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "amaterasu.log"
            log.write_text('Info: input.timing: {broken\nInfo: input.timing: ' +
                           json.dumps({"context_index": 1, "timing_us": {
                               "event_injection_us": {"calls": 1, "total_us": 2000, "max_us": 2000}}}),
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
        rows = summarize([parse_line('Info: amaterasu.rtx-hdr::rtx-hdr: ' + json.dumps(payload))])
        self.assertEqual(len(rows), 3)
        self.assertEqual(rows[0]["mean_us"], 4000)
        self.assertEqual(rows[0]["max_us"], 5000)
        self.assertIsNone(parse_line('Info: amaterasu.rtx-hdr::rtx-hdr: initialized'))

    def test_broker_rows_keep_service_identity_across_row_reordering(self):
        metric={"calls":1,"total_us":5000,"max_us":5000,"max_observed_monotonic_us":1234}
        services=[{"caller":"owner","service":name,"timing_us":{"queue_us":metric}} for name in ("a/1","b/1")]
        first=parse_line('Info: broker.timing: '+json.dumps({"services":services}))
        second=parse_line('Info: broker.timing: '+json.dumps({"services":list(reversed(services))}))
        rows=summarize([first,second])
        self.assertEqual(len(rows),2)
        self.assertTrue(all(row['calls']==2 for row in rows))
        self.assertTrue(all(row['max_observed_monotonic_us']==1234 for row in rows))
    def test_missing_boundary_is_never_reported_as_measured_zero(self):
        result=coverage([parse_line('Info: input.timing: '+json.dumps({"context_index":0}))])
        self.assertEqual(result['input_backend_calls'],'observed')
        self.assertEqual(result['protocol_reordering_and_reassembly'],'missing')
        self.assertEqual(result['kernel_receive_residence'],'unavailable_or_missing')
    def test_named_storage_stages_are_not_combined_by_array_position(self):
        metric={"calls":1,"total_us":20000,"max_us":20000}
        row=parse_line('Info: module.timing: '+json.dumps({"module_type":"state","stages":[{"stage":"storage.file_sync","timing_us":metric}]}))
        self.assertEqual(summarize([row])[0]['stage'],'stages.storage.file_sync.timing_us')


if __name__ == "__main__":
    unittest.main()
