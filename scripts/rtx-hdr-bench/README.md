# RTX HDR native latency harness

This opt-in NVIDIA hardware test links and calls the current production native
RTX HDR and NVENC sources. It supplies a real immutable GBM DMA-BUF with color
bars and produces real HEVC 10-bit packets. A warm-up readback rejects collapsed
color bars; every encoded output must be nonempty. Thirty warm-up frames are
excluded. The two-slot encoder and output thread permit actual overlapping work.

`source_to_bitstream` measures monotonic wall time immediately before RTX HDR
processing until NVENC locks that frame's encoded output. `dispatch` measures
CPU processing/submission. CUDA events measure vendor inference and the output
kernel individually. These intervals overlap: do not add them together.

This is a native-stage benchmark, not the complete streaming measurement. It
excludes capture acquisition/snapshot, SDK/video scheduling, time waiting for a
free input slot, transport admission, network, decoding and presentation. It
cannot by itself establish the cause of a live streaming regression. Color bars
also do not reproduce game GPU contention. The reported source hashes identify
exactly what was compiled, including dirty source changes.

Run only while streaming is idle. It does not start, stop or activate a service.
Build output must be on the approved build volume. For example, from the repo:

```sh
python scripts/rtx-hdr-bench/run.py \
  --runtime /absolute/path/to/licensed/runtime \
  --build-dir /run/media/chasep/ArchivedGames/worktrees/amaterasu/rtx-bench-build \
  --output /tmp/rtx-hdr-bench.json --frames 240 --fps 120 --peak 1424 --self-test
```

The normal run alternates historical/current/current/historical, then runs a
conversion bypass. Historical freezes the pre-fix mapping scope from RTX HDR
commit `6d537e4`; it is a control for that specific change, not the entire old
release. Current calls production `Session::run_cuda`. The harness wraps the
real vendor call and output-kernel dispatch only to record CUDA timestamps.
Debanding is enabled; model controls are contrast/saturation 100 and gray 50.

`--self-test` additionally runs the current real-code path with and without an
injected 5 ms delay before output completion is observed. It fails if the
average of paired p50 measurements increases by less than 3 ms. This validates
that the harness observes completion latency rather than merely submission.
The artificial delay is confined to this harness and never used in production.

Use `--fps 0 --frames 1200` for a quick saturation comparison. This changes the
workload: use paced runs for streaming-like latency and unpaced runs for capacity.
Use smaller even dimensions for faster smoke tests. Compare the same settings,
GPU workload and report scope; retain multiple runs because scheduling changes
can dominate small apparent improvements. No latency improvement is currently
proven for the existing mapping-scope candidate.

To compare a future change with a saved baseline, add
`--compare /tmp/rtx-hdr-bench.json --max-regression-percent 20` while writing
`--output` to a different path. The runner compares the average paired current
p95 values, checks matching GPU/scope/settings, and exits unsuccessfully when
it exceeds the chosen tolerance. A passing comparison is a workload-specific
regression check; it does not prove all streaming workloads improved.
