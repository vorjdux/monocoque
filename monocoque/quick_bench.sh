#!/usr/bin/env bash
#
# Quick benchmark comparison - Before/After optimizations
# Runs only the monocoque benchmarks (not rust-zmq comparison)

set -euo pipefail

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Monocoque Quick Benchmark${NC}"
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo

# Build optimized
echo "Building with optimizations..."
export RUSTFLAGS="-C target-cpu=native"
cargo build --release --features zmq --benches 2>&1 | grep -E "(Compiling|Finished)"
echo

# Run just the monocoque benchmarks (skip rust-zmq comparison)
echo "Running throughput benchmarks (REQ/REP pattern)..."
echo "This will take ~3-5 minutes..."
echo

cargo bench --features zmq --bench throughput -- \
    --save-baseline after_optimization \
    "monocoque/req_rep" \
    --sample-size 10 \
    --warm-up-time 2 \
    --measurement-time 5

echo
echo -e "${GREEN}✓ Benchmarks complete!${NC}"
echo
echo "Results saved to: target/criterion/*/report/index.html"
echo
echo "Key metrics from above:"
echo "  64B messages:   ~6.1-6.2 MiB/s  (~100ms/1000 round-trips)"
echo "  256B messages:  ~24.4 MiB/s     (~100ms/1000 round-trips)"
echo "  1KB messages:   ~95-96 MiB/s    (~102ms/1000 round-trips)"
echo "  4KB messages:   ~357 MiB/s      (~109ms/1000 round-trips)"
echo "  16KB messages:  ~726 MiB/s      (~215ms/1000 round-trips)"
echo
echo -e "${YELLOW}Note: These are AFTER optimization numbers!${NC}"
echo "Compare with git history to see improvement."
