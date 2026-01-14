#!/bin/bash
#
# Comprehensive benchmark runner for monocoque
#
# Runs all benchmarks and generates comparison reports with baseline tracking.
#
# Usage:
#   ./scripts/bench_all.sh                    # Run all benchmarks
#   ./scripts/bench_all.sh --save main        # Save baseline named "main"
#   ./scripts/bench_all.sh --compare main     # Compare against "main" baseline
#   ./scripts/bench_all.sh --quick            # Quick run (fewer samples)
#

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
BENCH_DIR="monocoque"
RESULTS_DIR="bench_results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULTS_FILE="${RESULTS_DIR}/summary_${TIMESTAMP}.md"

# Parse arguments
QUICK_MODE=false
SAVE_BASELINE=""
COMPARE_BASELINE=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --quick)
            QUICK_MODE=true
            shift
            ;;
        --save)
            SAVE_BASELINE="$2"
            shift 2
            ;;
        --compare)
            COMPARE_BASELINE="$2"
            shift 2
            ;;
        --help|-h)
            cat << EOF
Usage: $0 [OPTIONS]

Options:
    --quick              Run benchmarks with reduced samples (faster)
    --save BASELINE      Save results as baseline with given name
    --compare BASELINE   Compare results against named baseline
    --help, -h           Show this help message

Examples:
    $0                           # Run all benchmarks normally
    $0 --save main               # Save as "main" baseline
    $0 --compare main            # Compare against "main"
    $0 --quick --save feature    # Quick run and save as "feature"

EOF
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            exit 1
            ;;
    esac
done

# Create results directory
mkdir -p "${RESULTS_DIR}"

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}Monocoque Performance Benchmark Suite${NC}"
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""
echo -e "Timestamp: ${GREEN}$(date)${NC}"
echo -e "Quick mode: ${YELLOW}${QUICK_MODE}${NC}"
if [[ -n "$SAVE_BASELINE" ]]; then
    echo -e "Saving baseline: ${GREEN}${SAVE_BASELINE}${NC}"
fi
if [[ -n "$COMPARE_BASELINE" ]]; then
    echo -e "Comparing against: ${GREEN}${COMPARE_BASELINE}${NC}"
fi
echo ""

# Build in release mode first
echo -e "${BLUE}Building in release mode...${NC}"
cd "${BENCH_DIR}"
cargo build --release --features zmq
echo ""

# Run benchmarks
benchmarks=(
    "latency"
    "throughput"
    "pipelined_throughput"
    "ipc_vs_tcp"
    "multithreaded"
    "patterns"
)

echo -e "${BLUE}Running benchmarks...${NC}"
echo ""

for bench in "${benchmarks[@]}"; do
    echo -e "${GREEN}▸ Running $bench...${NC}"
    
    if [[ "$bench" == "ipc_vs_tcp" ]] && [[ "$(uname)" != "Linux" ]] && [[ "$(uname)" != "Darwin" ]]; then
        echo -e "${YELLOW}  ⚠ Skipping (Unix-only)${NC}"
        continue
    fi
    
    if $QUICK_MODE; then
        # Quick mode: fewer samples, shorter measurement time
        cargo bench --bench "$bench" --features zmq -- --sample-size 5 --measurement-time 5
    elif [[ -n "$SAVE_BASELINE" ]]; then
        # Save baseline mode
        cargo bench --bench "$bench" --features zmq -- --save-baseline "$SAVE_BASELINE"
    elif [[ -n "$COMPARE_BASELINE" ]]; then
        # Compare mode
        cargo bench --bench "$bench" --features zmq -- --baseline "$COMPARE_BASELINE"
    else
        # Normal mode
        cargo bench --bench "$bench" --features zmq
    fi
    echo ""
done

cd ..

# Generate summary report
echo -e "${BLUE}Generating summary report...${NC}"
echo ""

cat > "$RESULTS_FILE" << EOF
# Monocoque Benchmark Results

**Date**: $(date)  
**Git Commit**: $(git rev-parse --short HEAD 2>/dev/null || echo "N/A")  
**Git Branch**: $(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "N/A")  
**Quick Mode**: ${QUICK_MODE}

## System Information

- **OS**: $(uname -s)
- **Kernel**: $(uname -r)
- **CPU**: $(lscpu 2>/dev/null | grep "Model name" | cut -d: -f2 | xargs || sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Unknown")
- **Cores**: $(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "Unknown")
- **Memory**: $(free -h 2>/dev/null | awk '/^Mem:/{print $2}' || sysctl -n hw.memsize 2>/dev/null | awk '{print $1/1024/1024/1024 " GB"}' || echo "Unknown")

## Benchmark Results

Results are located in: \`${BENCH_DIR}/target/criterion/\`

### Latency Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/latency/\`
- **Measures**: Round-trip time in microseconds
- **Patterns**: REQ/REP connection establishment

### Throughput Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/throughput/\`
- **Measures**: Messages per second (synchronous ping-pong)
- **Patterns**: REQ/REP, DEALER/ROUTER
- **Comparison**: monocoque vs rust-zmq (zmq crate, FFI to libzmq)

### Pipelined Throughput Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/pipelined/\`
- **Measures**: Maximum throughput with decoupled send/recv
- **Target**: 500k-1M msg/sec for 64B messages
- **Tests**: Regular pipeline (10k messages), extreme pipeline (100k messages)

### IPC vs TCP Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/ipc_vs_tcp/\`
- **Measures**: Unix domain socket performance vs TCP loopback
- **Expected**: 40% latency improvement, 38% throughput improvement
- **Platform**: Unix-only (Linux, macOS)

### Multi-threaded Scaling Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/multithreaded/\`
- **Measures**: Horizontal scalability across CPU cores
- **Tests**: 
  - Multiple DEALER clients vs single ROUTER
  - Independent DEALER/ROUTER pairs
  - Core utilization efficiency
- **Target**: Linear scaling up to # of cores

### Pattern Benchmarks

- **Location**: \`${BENCH_DIR}/target/criterion/patterns/\`
- **Measures**: PUB/SUB fanout, topic filtering
- **Note**: Multi-subscriber fanout limited by current implementation

## How to View Results

### HTML Reports (Recommended)

Open in browser:
\`\`\`bash
firefox ${BENCH_DIR}/target/criterion/report/index.html
\`\`\`

### Command-line Comparison

\`\`\`bash
# Save current as baseline
./scripts/bench_all.sh --save main

# Make changes...

# Compare against baseline
./scripts/bench_all.sh --compare main
\`\`\`

### Criterion Output Format

Criterion provides:
- **Mean**: Average execution time
- **Std Dev**: Standard deviation
- **Median**: 50th percentile
- **MAD**: Median Absolute Deviation

## Performance Targets (from PERFORMANCE_ROADMAP.md)

### Phase 1 Goals (Current)

- ✅ Latency: 40-45µs (vs libzmq: ~125µs)
- ✅ Sync throughput: 130k msg/sec (vs libzmq: ~37k msg/sec)
- 🎯 Pipelined: 500k-1M msg/sec
- 🎯 IPC latency: ~30µs (40% faster than TCP)

### Phase 2 Goals (Next)

- 🚀 Latency: 35-40µs (with io_uring optimizations)
- 🚀 Pipelined: 1-2M msg/sec (with batching)

### Phase 3 Goals (Future)

- 🎯 Latency: 25-30µs (with zero-copy)
- 🎯 Pipelined: 2-5M msg/sec (with SIMD + zero-copy)
- 🎯 Bandwidth: 2+ GB/sec

## Notes

EOF

if $QUICK_MODE; then
    cat >> "$RESULTS_FILE" << EOF
⚠️ **Quick Mode Enabled**: Results use reduced samples and may be less accurate.
Use full benchmarks for authoritative measurements.

EOF
fi

if [[ -n "$COMPARE_BASELINE" ]]; then
    cat >> "$RESULTS_FILE" << EOF
### Comparison Against Baseline: ${COMPARE_BASELINE}

Check criterion reports for detailed comparisons with confidence intervals.

EOF
fi

cat >> "$RESULTS_FILE" << EOF
---

**Report generated**: $(date)  
**Monocoque version**: $(grep '^version' monocoque/Cargo.toml | head -1 | cut -d'"' -f2)

EOF

echo -e "${GREEN}✓ Summary report generated: ${RESULTS_FILE}${NC}"
echo ""

# Display key metrics if available
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${GREEN}Benchmark complete!${NC}"
echo ""
echo -e "Results saved to:"
echo -e "  • Summary: ${GREEN}${RESULTS_FILE}${NC}"
echo -e "  • Detailed: ${GREEN}${BENCH_DIR}/target/criterion/${NC}"
echo -e "  • HTML Report: ${GREEN}${BENCH_DIR}/target/criterion/report/index.html${NC}"
echo ""

if command -v xdg-open &> /dev/null; then
    read -p "Open HTML report in browser? (y/N) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        xdg-open "${BENCH_DIR}/target/criterion/report/index.html"
    fi
fi

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
