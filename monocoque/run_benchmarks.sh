#!/usr/bin/env bash
#
# Run all Monocoque benchmarks and generate comparison report
#
# Usage:
#   ./run_benchmarks.sh [baseline_name]
#
# Examples:
#   ./run_benchmarks.sh                    # Run benchmarks, compare to previous
#   ./run_benchmarks.sh v0.1.0            # Save baseline as v0.1.0
#   ./run_benchmarks.sh --compare v0.1.0  # Compare against v0.1.0

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
BENCH_DIR="target/criterion"
REPORT_DIR="bench_results"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

print_header() {
    echo -e "${BLUE}════════════════════════════════════════════════${NC}"
    echo -e "${BLUE}  $1${NC}"
    echo -e "${BLUE}════════════════════════════════════════════════${NC}"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

check_prerequisites() {
    print_header "Checking Prerequisites"
    
    # Check for cargo
    if ! command -v cargo &> /dev/null; then
        print_error "cargo not found. Install Rust: https://rustup.rs/"
        exit 1
    fi
    print_success "cargo found: $(cargo --version)"
    
    # Check for kernel version (io_uring requires 5.6+)
    if [[ -f /proc/version ]]; then
        kernel_version=$(uname -r)
        print_success "Linux kernel: $kernel_version"
        
        major=$(echo "$kernel_version" | cut -d. -f1)
        minor=$(echo "$kernel_version" | cut -d. -f2)
        if [[ $major -lt 5 ]] || [[ $major -eq 5 && $minor -lt 6 ]]; then
            print_warning "io_uring requires kernel 5.6+, you have $kernel_version"
            print_warning "Benchmarks may not show full io_uring benefits"
        fi
    fi
    
    # Check CPU governor
    if [[ -f /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]]; then
        governor=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)
        if [[ "$governor" != "performance" ]]; then
            print_warning "CPU governor is '$governor' (recommended: 'performance')"
            echo "  Run: sudo cpupower frequency-set --governor performance"
        else
            print_success "CPU governor: $governor"
        fi
    fi
    
    echo
}

build_optimized() {
    print_header "Building with Optimizations"
    
    export RUSTFLAGS="-C target-cpu=native"
    unset RUST_LOG
    print_success "RUSTFLAGS: $RUSTFLAGS"
    print_success "RUST_LOG: unset (clean benchmarks)"
    
    cargo build --release --benches --features zmq
    print_success "Build complete"
    echo
}

run_benchmark_suite() {
    local bench_name=$1
    local baseline_arg=$2
    
    print_header "Running: $bench_name"
    echo "  This may take several minutes..."
    echo "  Watch for Criterion progress below:"
    echo
    
    if [[ -n "$baseline_arg" ]]; then
        cargo bench --bench "$bench_name" --features zmq -- "$baseline_arg" --verbose
    else
        cargo bench --bench "$bench_name" --features zmq -- --verbose
    fi
    
    print_success "$bench_name complete"
    echo
}

generate_summary() {
    print_header "Generating Summary Report"
    
    mkdir -p "$REPORT_DIR"
    report_file="$REPORT_DIR/summary_${TIMESTAMP}.md"
    
    cat > "$report_file" << EOF
# Monocoque Benchmark Results

**Date**: $(date)
**Commit**: $(git rev-parse --short HEAD 2>/dev/null || echo "unknown")
**Rust**: $(rustc --version)
**Kernel**: $(uname -r)

## System Information

- **CPU**: $(grep "model name" /proc/cpuinfo | head -1 | cut -d: -f2 | xargs || echo "unknown")
- **Cores**: $(nproc || echo "unknown")
- **Memory**: $(free -h | awk '/^Mem:/ {print $2}' || echo "unknown")
- **CPU Governor**: $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "unknown")

## Benchmark Suites

EOF
    
    # Extract key results from criterion output
    if [[ -d "$BENCH_DIR" ]]; then
        for bench in throughput latency patterns pipelined_throughput ipc_vs_tcp multithreaded; do
            if [[ -d "$BENCH_DIR/$bench" ]]; then
                echo "### $bench" >> "$report_file"
                echo "" >> "$report_file"
                
                # Find latest results
                for result_dir in "$BENCH_DIR/$bench"/*/*; do
                    if [[ -f "$result_dir/base/estimates.json" ]]; then
                        test_name=$(basename "$(dirname "$result_dir")")
                        echo "- **$test_name**: See HTML report" >> "$report_file"
                    fi
                done
                
                echo "" >> "$report_file"
            fi
        done
    fi
    
    cat >> "$report_file" << EOF

## HTML Reports

Open in browser: \`file://$(pwd)/$BENCH_DIR/report/index.html\`

## Comparison

### Monocoque vs rust-zmq

| Benchmark | Monocoque | rust-zmq | Speedup |
|-----------|-----------|--------|---------|
| REQ/REP 256B | TBD | TBD | TBD |
| DEALER/ROUTER 1KB | TBD | TBD | TBD |
| PUB/SUB fanout (10) | TBD | TBD | TBD |
| Pipelined (10k msgs) | TBD | TBD | TBD |

### New Benchmarks

- **Pipelined Throughput**: Tests batched send/receive with explicit flush API
- **IPC vs TCP**: Compares Unix domain sockets vs TCP loopback performance
- **Multithreaded**: Tests horizontal scalability across CPU cores

_(Extract detailed results from HTML reports)_

## Notes

- All benchmarks use local TCP connections (127.0.0.1)
- Message sizes range from 8B to 16KB
- Each benchmark runs multiple iterations for statistical significance
- Results may vary based on system load and configuration

EOF
    
    print_success "Report saved: $report_file"
    echo
}

open_report() {
    print_header "Opening HTML Report"
    
    html_report="$BENCH_DIR/report/index.html"
    
    if [[ -f "$html_report" ]]; then
        if command -v xdg-open &> /dev/null; then
            xdg-open "$html_report" &
            print_success "Opened in browser"
        elif command -v open &> /dev/null; then
            open "$html_report" &
            print_success "Opened in browser"
        else
            print_warning "Cannot open browser automatically"
            echo "  Open manually: file://$(pwd)/$html_report"
        fi
    else
        print_warning "HTML report not found"
    fi
    
    echo
}

main() {
    local baseline_arg=""
    local save_baseline=false
    
    # Parse arguments
    if [[ $# -gt 0 ]]; then
        if [[ "$1" == "--compare" ]]; then
            if [[ $# -lt 2 ]]; then
                print_error "Usage: $0 --compare <baseline_name>"
                exit 1
            fi
            baseline_arg="--baseline $2"
        else
            baseline_arg="--save-baseline $1"
            save_baseline=true
        fi
    fi
    
    echo
    print_header "Monocoque Performance Benchmarks"
    echo
    
    check_prerequisites
    build_optimized
    
    # Run benchmark suites
    run_benchmark_suite "throughput" "$baseline_arg"
    run_benchmark_suite "latency" "$baseline_arg"
    run_benchmark_suite "patterns" "$baseline_arg"
    run_benchmark_suite "pipelined_throughput" "$baseline_arg"
    run_benchmark_suite "ipc_vs_tcp" "$baseline_arg"
    run_benchmark_suite "multithreaded" "$baseline_arg"
    
    generate_summary
    open_report
    
    print_header "Benchmarks Complete!"
    
    if [[ "$save_baseline" == true ]]; then
        print_success "Baseline saved as: $1"
        echo "  Compare against it: ./run_benchmarks.sh --compare $1"
    fi
    
    echo
}

main "$@"
