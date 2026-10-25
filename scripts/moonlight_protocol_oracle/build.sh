#!/bin/sh
# Build against the developer-supplied Moonlight protocol source, without editing it.
set -eu
if [ "$#" != 2 ]; then
    echo "usage: $0 MOONLIGHT_COMMON_C_CHECKOUT OUTPUT_DIRECTORY" >&2
    exit 2
fi
source_dir=$(realpath "$1")
output_dir=$(realpath -m "$2")
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$output_dir/" in "$source_dir/"*) echo 'output must be outside the source checkout' >&2; exit 2;; esac
mkdir -p "$output_dir"
git -C "$source_dir" rev-parse HEAD > "$output_dir/source-commit.txt"
git -C "$source_dir" status --short > "$output_dir/source-status.txt"
cmake -S "$source_dir" -B "$output_dir/library" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON
cmake --build "$output_dir/library" --parallel 4
# pkg-config produces compiler arguments, intentionally word-split here.
# shellcheck disable=SC2046
cc -std=c11 -O2 -Wall -Wextra -Werror -I"$source_dir/src" \
    "$script_dir/client.c" -L"$output_dir/library" -Wl,-rpath,"$output_dir/library" \
    -lmoonlight-common-c $(pkg-config --cflags --libs opus libavcodec libavutil) \
    -o "$output_dir/client"
printf 'Built %s/client\n' "$output_dir"
