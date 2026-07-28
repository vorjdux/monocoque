#!/bin/bash
# Run every fuzz target for a fixed time budget each.
#
# Usage:
#   scripts/run_fuzzer.sh [SECONDS_PER_TARGET] [TARGET ...]
#
#   scripts/run_fuzzer.sh            # all targets, 10s each
#   scripts/run_fuzzer.sh 60         # all targets, 60s each
#   scripts/run_fuzzer.sh 30 fuzz_decoder fuzz_greeting   # named targets, 30s each
#
# The target list is discovered from `cargo fuzz list`, so adding or removing a
# fuzz target needs no edit here - the loop always covers exactly what exists.

set -euo pipefail

cd "$(dirname "$0")/.."

FUZZ_DIR="monocoque-fuzz"
TIME="${1:-10}"
shift || true

# Remaining args, if any, are an explicit target list; otherwise fuzz them all.
if [ "$#" -gt 0 ]; then
    targets=("$@")
else
    mapfile -t targets < <(cargo +nightly fuzz list --fuzz-dir "$FUZZ_DIR")
fi

if [ "${#targets[@]}" -eq 0 ]; then
    echo "No fuzz targets found under ${FUZZ_DIR}." >&2
    exit 1
fi

echo "Fuzzing ${#targets[@]} target(s) for ${TIME}s each: ${targets[*]}"

failed=()
for t in "${targets[@]}"; do
    echo "::group::${t}"
    if cargo +nightly fuzz run --fuzz-dir "$FUZZ_DIR" "$t" -- \
        -max_total_time="${TIME}" -rss_limit_mb=4096; then
        echo "clean: ${t}"
    else
        echo "crash: ${t}"
        failed+=("$t")
    fi
    echo "::endgroup::"
done

if [ "${#failed[@]}" -gt 0 ]; then
    echo "Fuzzing failed for: ${failed[*]}" >&2
    exit 1
fi

echo "All fuzz targets clean."
