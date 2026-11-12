# Capture presentation timing

Capture v1.12 adds a nonblocking `frame_timing(stream, sequence, output)` query
following the complete v1.11 source-preflight prefix. The abandoned v1.11 timing
draft must not be used: v1.11 already belongs to source preflight. Consumers
check both minor version and table size before accessing the new tail.

The record identifies the exact presentation and when its provider began
handling it on the host monotonic clock, before export or snapshot submission.
Zero, an evicted record, refusal, a busy provider, or an older table means
unknown. Video retains its own provisional measurement in these cases. A valid
provider start must precede that local start and fit the existing latency sanity
window; encoded output freezes the selected start for every packet of a frame.
This measures host processing and does not establish game-to-photon latency.

The VKMS provider stores a bounded history. Its optional immutable GPU snapshot
path reuses four destination buffers only after their stored and retained leases
and completion obligations have ended. Source imports are bounded and cached by
owned DMA-BUF identity. Source guards and a destination claim survive failed GPU
submissions until native completion is proven; failure to prove retirement keeps
them quarantined and prevents successful provider retirement. Copy completion
is polled on the presentation worker, never waited on the host tick.
