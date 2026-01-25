#!/bin/bash
# Run fuzzer with correct directory path

set -e

cd "$(dirname "$0")/.."

# Default to 10 seconds if no time specified
TIME="${1:-10}"

echo "Building fuzzer..."
cargo +nightly fuzz build --fuzz-dir monocoque-fuzz fuzz_decoder

echo "Running fuzzer for ${TIME} seconds..."
cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_decoder -- -max_total_time="${TIME}"
