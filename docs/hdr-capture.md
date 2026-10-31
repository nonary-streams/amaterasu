# Linux guarded HDR capture

The VKMS capture plugin, native NVENC/CUDA encoder and streaming modules support
HDR10 on the owned Linux virtual-output path. Native modules are trusted code.
The shell does not perform capture, colour conversion or HDR policy.

The driver publishes colour state and a copied HDR metadata blob with the same
completed presentation as the exported framebuffer. Helper protocol 3 requests
`GET_FRAME_COLOR` with a read guard. Capture copies RGB10 into private GPU storage,
waits for the copy, destroys the source import and releases the source guard.
The encoder retains the private snapshot, so queued encoding does not hold the
compositor's scanout.

For HDR, only known PQ/BT.2020 ten-bit RGB is accepted. The NVENC/CUDA backend
converts to limited-range BT.2020 non-constant-luminance P010 on the GPU, preserving
ten bits, and signals PQ, BT.2020 and Main 10 HEVC or ten-bit AV1. It rejects SDR
input into an HDR session and other encoders without this path reject HDR. SDR
encoding remains available. No CPU readback or implicit eight-bit fallback is
used for the required-lease HDR composition.

Capture v1.9 and video v1.2 add static metadata queries without changing older
native prefixes. The stream-session module sends the current HDR mode and static
metadata on the reliable Moonlight control channel. NVHTTP advertises HDR10 only
from an encoder explicitly reporting that capability, narrows an HDR launch to
those encoders, and freezes the request into the RTSP profile. RTSP admits only
the matching negotiated dynamic range. An owned KDE output can be created with
HDR and wide colour enabled; readback is required and the existing restoration
path restores prior display state.

Build capture with `driver-read-leases,linux-kwin-geometry,linux-gpu-snapshot` and
the helper with `driver-read-leases`. Configure capture with `gpu_snapshot:true`
and `require_lease:true`, and video with `require_frame_lease:true`. Use the native
encoder's `linux-nvenc-cuda` feature. The matching colour-export driver must be
loaded, not merely installed on disk. HDR requests select HEVC/AV1, ten-bit, HDR
through `video_stream_format`; unknown source colour is not inferred from a
fourcc alone.

Validation must distinguish a synthetic RGB10 encoder test from guarded real
capture and client playback. The opt-in `nvenc_cuda_gpu_hdr10` test encodes real
GPU RGB10 DMA-BUFs, decodes both codecs and checks colour signalling and samples.
An installed qualification additionally needs a real HDR output, matching loaded
driver identity, frame-lease diagnostics, full decode, metadata propagation and
cleanup. Client display playback and content accuracy need a client test. This
path does not implement scRGB/FP16 conversion, HDR-to-SDR tone mapping, other GPU
encoder backends, or safety for arbitrarily retained direct scanout imports.
