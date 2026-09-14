#!/bin/sh
# Build only an explicitly supplied, verified upstream checkout; no downloads.
set -eu
nanors_source=${1:?usage: build-oracle.sh NANORS_CHECKOUT OUTPUT_EXECUTABLE}
oracle_output=${2:?usage: build-oracle.sh NANORS_CHECKOUT OUTPUT_EXECUTABLE}
oracle_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
expected_commit=b1e3c22ca0cdc0bb83e3cd6ed1a2fc77869ed99a
actual_commit=$(git -C "$nanors_source" rev-parse HEAD)
if [ "$actual_commit" != "$expected_commit" ]; then
    echo "nanors checkout does not match the qualified commit" >&2
    exit 1
fi
git -C "$nanors_source" diff --quiet HEAD --
# A detached source copy without Git metadata is intentionally not accepted.
cc -std=c11 -O2 -DOBLAS_TINY \
    -I"$nanors_source" -I"$nanors_source/deps/obl" \
    "$oracle_directory/oracle.c" \
    "$nanors_source/rs.c" \
    "$nanors_source/deps/obl/oblas_common.c" \
    "$nanors_source/deps/obl/oblas_lite.c" \
    -o "$oracle_output"
