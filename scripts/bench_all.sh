#!/bin/bash
#
# Comprehensive benchmark runner for monocoque
#
# Runs the criterion benchmarks on one or all runtime backends, optionally
# pinned to chosen cores, with baseline tracking.
#
# Usage:
#   ./scripts/bench_all.sh                        # compio, all benchmarks
#   ./scripts/bench_all.sh --backend all          # compio, tokio and smol
#   ./scripts/bench_all.sh --pin 0,2              # pin the two-thread benches
#   ./scripts/bench_all.sh --bench throughput     # one benchmark only
#   ./scripts/bench_all.sh --save main            # save baseline named "main"
#   ./scripts/bench_all.sh --compare main         # compare against "main"
#   ./scripts/bench_all.sh --quick                # shorter, less accurate run
#
# A saved baseline is compared with a run made at another time, so anything
# that changed on the machine in between shows up as a difference. To compare
# two revisions without that, use scripts/bench_pair.py.
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
# Criterion writes to the workspace target dir, not the package dir.
CRITERION_DIR="target/criterion"
RESULTS_DIR="bench_results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULTS_FILE="${RESULTS_DIR}/summary_${TIMESTAMP}.md"
LOAD_FILE="${RESULTS_DIR}/load_${TIMESTAMP}.log"

ALL_BENCHMARKS=(
    "latency"
    "throughput"
    "pipelined_throughput"
    "ipc_vs_tcp"
    "multithreaded"
    "patterns"
    "fanout_fanin"
    "fanout_slow_subscriber"
)

# These run a sender and a receiver, one thread each, so they can be pinned to
# two cores. The others start more threads than that and run unpinned.
TWO_THREAD_BENCHMARKS=" latency throughput pipelined_throughput ipc_vs_tcp "

# Parse arguments
QUICK_MODE=false
SAVE_BASELINE=""
COMPARE_BASELINE=""
BACKEND="compio"
PIN=""
benchmarks=()

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
        --backend)
            BACKEND="$2"
            shift 2
            ;;
        --pin)
            PIN="$2"
            shift 2
            ;;
        --bench)
            benchmarks+=("$2")
            shift 2
            ;;
        --help|-h)
            cat << EOF
Usage: $0 [OPTIONS]

Options:
    --backend NAME       compio (default), tokio, smol, or all
    --pin CORES          Pin the two-thread benchmarks to these cores, for
                         example 0,2. Choose two separate physical cores, not
                         two hyperthreads of one (see: lscpu -e)
    --bench NAME         Run only this benchmark; may be given more than once
                         (${ALL_BENCHMARKS[*]})
    --quick              Shorter run, less accurate
    --save BASELINE      Save results as baseline with given name
    --compare BASELINE   Compare results against named baseline
    --help, -h           Show this help message

Examples:
    $0                               # compio, all benchmarks
    $0 --backend all --pin 0,2       # every backend, pinned
    $0 --save main                   # Save as "main" baseline
    $0 --compare main                # Compare against "main"
    $0 --bench throughput --quick    # Quick look at one benchmark

EOF
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            exit 1
            ;;
    esac
done

case "$BACKEND" in
    compio|tokio|smol) backends=("$BACKEND") ;;
    all) backends=(compio tokio smol) ;;
    *)
        echo -e "${RED}Unknown backend: ${BACKEND} (compio, tokio, smol or all)${NC}"
        exit 1
        ;;
esac

if [[ ${#benchmarks[@]} -eq 0 ]]; then
    benchmarks=("${ALL_BENCHMARKS[@]}")
fi
for bench in "${benchmarks[@]}"; do
    if [[ " ${ALL_BENCHMARKS[*]} " != *" $bench "* ]]; then
        echo -e "${RED}Unknown benchmark: ${bench} (${ALL_BENCHMARKS[*]})${NC}"
        exit 1
    fi
done

if [[ -n "$PIN" ]]; then
    if ! command -v taskset &> /dev/null; then
        echo -e "${RED}--pin needs taskset (util-linux)${NC}"
        exit 1
    fi
    if ! taskset -c "$PIN" true 2> /dev/null; then
        echo -e "${RED}Cannot pin to cores ${PIN}${NC}"
        exit 1
    fi
fi

backend_features() {
    case "$1" in
        compio) echo "--features zmq" ;;
        tokio) echo "--no-default-features --features runtime-tokio,zmq" ;;
        smol) echo "--no-default-features --features runtime-smol,zmq" ;;
    esac
}

# Create results directory
mkdir -p "${RESULTS_DIR}"

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BLUE}Monocoque Performance Benchmark Suite${NC}"
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""
echo -e "Timestamp: ${GREEN}$(date)${NC}"
echo -e "Backends: ${GREEN}${backends[*]}${NC}"
echo -e "Benchmarks: ${GREEN}${benchmarks[*]}${NC}"
echo -e "Pinned to cores: ${GREEN}${PIN:-not pinned}${NC}"
echo -e "Quick mode: ${YELLOW}${QUICK_MODE}${NC}"
if [[ -n "$SAVE_BASELINE" ]]; then
    echo -e "Saving baseline: ${GREEN}${SAVE_BASELINE}${NC}"
fi
if [[ -n "$COMPARE_BASELINE" ]]; then
    echo -e "Comparing against: ${GREEN}${COMPARE_BASELINE}${NC}"
fi
echo ""

# Criterion arguments for the chosen mode
criterion_args=()
if $QUICK_MODE; then
    # Ten samples is the fewest criterion accepts. A benchmark that sets its
    # own measurement time keeps it; BENCH_SECS shortens those that read it.
    criterion_args+=(--sample-size 10 --measurement-time 3 --warm-up-time 1)
    export BENCH_SECS="${BENCH_SECS:-3}"
fi
if [[ -n "$SAVE_BASELINE" ]]; then
    criterion_args+=(--save-baseline "$SAVE_BASELINE")
elif [[ -n "$COMPARE_BASELINE" ]]; then
    # Lenient, so a case the baseline does not have (one added since it was
    # saved) is measured without a comparison instead of stopping the run.
    criterion_args+=(--baseline-lenient "$COMPARE_BASELINE")
fi

# Record the machine's load through the run, to tell a quiet run from a
# disturbed one afterwards.
if [[ -r /proc/loadavg ]]; then
    (while true; do
        echo "$(date +%T) $(cut -d' ' -f1-3 /proc/loadavg)"
        sleep 30
    done) > "$LOAD_FILE" &
    LOAD_PID=$!
    trap 'kill "$LOAD_PID" 2> /dev/null || true' EXIT
fi

cd "${BENCH_DIR}"

first_backend=true
for backend in "${backends[@]}"; do
    read -r -a features <<< "$(backend_features "$backend")"

    # Build before measuring, so compiling is neither timed nor pinned.
    echo -e "${BLUE}Building benchmarks for ${backend}...${NC}"
    for bench in "${benchmarks[@]}"; do
        cargo bench --bench "$bench" "${features[@]}" --no-run
    done
    echo ""

    # The libzmq cases do not depend on the backend: run them once, in the
    # first pass, and only monocoque's own cases after that.
    filter=()
    if ! $first_backend; then
        filter=("monocoque")
    fi

    echo -e "${BLUE}Running benchmarks on ${backend}...${NC}"
    echo ""

    for bench in "${benchmarks[@]}"; do
        echo -e "${GREEN}▸ Running $bench ($backend)...${NC}"

        if [[ "$bench" == "ipc_vs_tcp" ]] && [[ "$(uname)" != "Linux" ]] && [[ "$(uname)" != "Darwin" ]]; then
            echo -e "${YELLOW}  ⚠ Skipping (Unix-only)${NC}"
            continue
        fi

        runner=()
        if [[ -n "$PIN" ]] && [[ "$TWO_THREAD_BENCHMARKS" == *" $bench "* ]]; then
            runner=(taskset -c "$PIN")
        fi

        # The ${a[@]+...} form is for bash before 4.4, where expanding an
        # empty array under `set -u` is an error.
        ${runner[@]+"${runner[@]}"} cargo bench --bench "$bench" "${features[@]}" -- \
            ${filter[@]+"${filter[@]}"} ${criterion_args[@]+"${criterion_args[@]}"}
        echo ""
    done

    first_backend=false
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
**Backends**: ${backends[*]}
**Benchmarks**: ${benchmarks[*]}
**Pinned to cores**: ${PIN:-not pinned}
**Quick Mode**: ${QUICK_MODE}

## System Information

- **OS**: $(uname -s)
- **Kernel**: $(uname -r)
- **CPU**: $(lscpu 2>/dev/null | grep "Model name" | cut -d: -f2 | xargs || sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Unknown")
- **Cores**: $(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo "Unknown")
- **Memory**: $(free -h 2>/dev/null | awk '/^Mem:/{print $2}' || sysctl -n hw.memsize 2>/dev/null | awk '{print $1/1024/1024/1024 " GB"}' || echo "Unknown")
- **Load during the run**: \`${LOAD_FILE}\` (1, 5 and 15 minute averages, every 30 s)

## Benchmark Results

Results are located in: \`${CRITERION_DIR}/\`

Each monocoque case is named after its backend, for example
\`throughput/monocoque-tokio/push_pull\`.

### Latency Benchmarks

- **Location**: \`${CRITERION_DIR}/latency/\`
- **Measures**: Steady-state REQ/REP round-trip time on a persistent connection
- **Comparison**: monocoque vs rust-zmq (zmq crate, FFI to libzmq)

### Throughput Benchmarks

- **Location**: \`${CRITERION_DIR}/throughput/\`
- **Measures**: Messages per second, PUSH/PULL one-way, eager and coalesced
- **Comparison**: monocoque vs rust-zmq (zmq crate, FFI to libzmq)

### Pipelined Throughput Benchmarks

- **Location**: \`${CRITERION_DIR}/pipelined/\`
- **Measures**: DEALER/ROUTER echo in coalesced batches of 100, 10k messages

### IPC vs TCP Benchmarks

- **Location**: \`${CRITERION_DIR}/ipc_vs_tcp/\`
- **Measures**: Unix domain socket vs TCP loopback, latency and throughput
- **Platform**: Unix-only (Linux, macOS)

### Multi-threaded Scaling Benchmarks

- **Location**: \`${CRITERION_DIR}/multithreaded/\`
- **Measures**: Aggregate throughput of independent DEALER/ROUTER pairs, one
  pair per thread

### Pattern Benchmarks

- **Location**: \`${CRITERION_DIR}/patterns/\`
- **Measures**: PUB/SUB delivered throughput and topic filtering
- **Comparison**: monocoque vs rust-zmq (zmq crate, FFI to libzmq)

### Fan-out and Fan-in Benchmarks

- **Location**: \`${CRITERION_DIR}/fanout_fanin/\`
- **Measures**: One PUSH socket spreading work across a pool of workers, and
  one PULL socket merging a pool of senders

### Slow Subscriber Benchmarks

- **Location**: \`${CRITERION_DIR}/fanout_slow_subscriber/\`
- **Measures**: Delivery to the fast subscribers of a PUB socket while one
  subscriber of 100 is slow

## How to View Results

### HTML Reports (Recommended)

Open in browser:
\`\`\`bash
firefox ${CRITERION_DIR}/report/index.html
\`\`\`

### Command-line Comparison

\`\`\`bash
# Save current as baseline
./scripts/bench_all.sh --save main

# Make changes...

# Compare against baseline
./scripts/bench_all.sh --compare main
\`\`\`

A baseline and the run compared with it are measured at different times. For
two revisions measured in alternating rounds, which cancels what changes on
the machine in between:

\`\`\`bash
./scripts/bench_pair.py --against v0.4.2 --backend all --pin 0,2
\`\`\`

### Criterion Output Format

Criterion provides:
- **Mean**: Average execution time
- **Std Dev**: Standard deviation
- **Median**: 50th percentile
- **MAD**: Median Absolute Deviation

## Reference Numbers

See \`monocoque/BENCHMARKS.md\` and \`docs/performance.md\` for published
results.

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
A case the baseline does not contain is reported without a comparison.

EOF
fi

cat >> "$RESULTS_FILE" << EOF
---

**Report generated**: $(date)
**Monocoque version**: $(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)

EOF

echo -e "${GREEN}✓ Summary report generated: ${RESULTS_FILE}${NC}"
echo ""

# Display key metrics if available
echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${GREEN}Benchmark complete!${NC}"
echo ""
echo -e "Results saved to:"
echo -e "  • Summary: ${GREEN}${RESULTS_FILE}${NC}"
echo -e "  • Load log: ${GREEN}${LOAD_FILE}${NC}"
echo -e "  • Detailed: ${GREEN}${CRITERION_DIR}/${NC}"
echo -e "  • HTML Report: ${GREEN}${CRITERION_DIR}/report/index.html${NC}"
echo ""

# Only offer to open the report from a terminal with a display, never on a
# server or in a script.
if [[ -t 0 ]] && [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]] && command -v xdg-open &> /dev/null; then
    read -p "Open HTML report in browser? (y/N) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        xdg-open "${CRITERION_DIR}/report/index.html"
    fi
fi

echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
