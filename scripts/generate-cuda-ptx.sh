#!/bin/sh
# Regenerates the checked-in PTX for the CUDA resource-handoff kernels. The
# driver JIT-compiles this PTX at load, so builds need no CUDA compiler.
# compute_75 keeps every RTX-capable GPU (Turing and newer) supported.
set -eu
nvcc=${NVCC:-/opt/cuda/bin/nvcc}
root=$(cd "$(dirname "$0")/.." && pwd)
for kernel in \
    modules/rtx-hdr/src/native/cuda/output \
    modules/encode-video/encode/src/platform/nvenc_cuda/native/cuda/convert; do
    "$nvcc" --ptx -arch=compute_75 -O3 -ftz=false -prec-div=true -prec-sqrt=true \
        -o "$root/$kernel.ptx" "$root/$kernel.cu"
done
