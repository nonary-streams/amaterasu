#!/bin/sh
set -eu
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: $0 PINNED_ENET_CHECKOUT [OUTPUT]" >&2
    exit 2
fi
source_dir=$(cd "$1" && pwd)
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
output=${2:-/private/tmp/amaterasu-enet-client-oracle}
expected=aca87840b57f045a1f7f9299e4b1b9b8e2a5e2f1
actual=$(git -C "$source_dir" rev-parse HEAD)
if [ "$actual" != "$expected" ]; then
    echo "ENet source commit mismatch: expected $expected, got $actual" >&2
    exit 1
fi
git -C "$source_dir" diff --quiet HEAD -- || {
    echo "ENet tracked source has modifications" >&2
    exit 1
}
build_dir=$(mktemp -d /private/tmp/amaterasu-enet-oracle.XXXXXX)
trap 'rm -rf "$build_dir"' EXIT HUP INT TERM
"${CMAKE:-cmake}" -S "$source_dir" -B "$build_dir" -DENET_NO_INSTALL=ON -DCMAKE_BUILD_TYPE=Release
"${CMAKE:-cmake}" --build "$build_dir" --target enet --parallel 2
"${CC:-cc}" -std=c11 -O2 -Wall -Wextra -Werror -I"$source_dir/include" \
    "$script_dir/client.c" "$build_dir/libenet.a" -o "$output"
printf 'Built %s against ENet %s\n' "$output" "$actual"
