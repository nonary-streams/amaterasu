import unittest

from analyze_rtx_hdr_trace import analyze


def event(thread, time, name, phase):
    return f"{thread} [001] .... {time:.6f}: {name}_{phase}: (0x1)\n"


class TraceAnalysisTests(unittest.TestCase):
    def test_tracks_mapping_per_thread_and_does_not_count_gpu_time(self):
        lines = [
            event("video-1", 1.000, "cuGraphicsMapResources", "begin"),
            event("video-1", 1.001, "cuGraphicsMapResources", "end"),
            event("video-2", 1.001, "NvVFX_Run", "begin"),
            event("video-1", 1.002, "NvVFX_Run", "begin"),
            event("video-1", 1.003, "NvVFX_Run", "end"),
            event("video-1", 1.004, "cuGraphicsUnmapResources", "begin"),
            event("video-1", 1.005, "cuGraphicsUnmapResources", "end"),
            event("video-1", 1.006, "NvVFX_Run", "begin"),
            event("video-1", 1.007, "NvVFX_Run", "end"),
        ]
        result = analyze(lines)
        self.assertEqual(result["model_calls_mapped"], 1)
        self.assertEqual(result["model_calls_after_unmap"], 1)
        self.assertEqual(result["model_calls_unknown"], 1)
        self.assertEqual(result["incomplete_calls"], 1)
        self.assertEqual(result["map_to_unmap_cpu_us"]["median"], 4000)
        self.assertEqual(result["model_call_cpu_us"]["median"], 1000)

    def test_truncated_trace_does_not_invent_mapping(self):
        result = analyze([event("video-1", 1, "cuGraphicsMapResources", "end"),
                          event("video-1", 2, "NvVFX_Run", "begin")])
        self.assertEqual(result["model_calls_unknown"], 1)
        self.assertEqual(result["incomplete_calls"], 2)


if __name__ == "__main__":
    unittest.main()
