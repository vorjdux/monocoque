# Monocoque Performance Benchmarks

Comprehensive performance benchmarks comparing Monocoque against rust-zmq (zmq crate, Rust FFI bindings to libzmq) and other Rust ZMQ implementations.

## Objective

**Outperform existing Rust ZMQ libraries** through:

-   Zero-copy message passing with `Bytes`
-   io_uring-based I/O (Linux only, requires kernel 5.6+)
-   Arena allocation for read buffers
-   Optimized subscription indexing (sorted Vec, cache-friendly)
-   Minimal overhead socket types

## Benchmark Suites

### 1. Throughput (`cargo bench --bench throughput`)

**Measures**: Messages per second

**Tests**:

-   REQ/REP throughput (64B to 16KB messages)
-   DEALER/ROUTER throughput (64B to 16KB messages)
-   Compares monocoque vs rust-zmq

**Target**: > 1M msg/sec for small messages

### 2. Latency (`cargo bench --bench latency`)

**Measures**: Round-trip time in microseconds

**Tests**:

-   REQ/REP latency (8B to 4KB messages)
-   Connection establishment latency
-   Single message round-trip time

**Target**: < 10μs latency for local connections

### 3. Patterns (`cargo bench --bench patterns`)

**Measures**: Pattern-specific performance

**Tests**:

-   PUB/SUB fanout (1 → N subscribers)
-   Topic filtering efficiency
-   Subscription matching overhead

**Target**: Linear scaling with subscriber count

## Running Benchmarks

### Quick Start

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark suite
cargo bench --bench throughput
cargo bench --bench latency
cargo bench --bench patterns

# Run with filtering
cargo bench throughput/monocoque  # Only monocoque tests
cargo bench latency/rust_zmq      # Only rust-zmq tests
```

### Optimal Configuration

For accurate results:

```bash
# Build with full optimizations
export RUSTFLAGS="-C target-cpu=native"

# Run benchmarks in release mode (default)
cargo bench --bench throughput

# Save baseline for comparison
cargo bench --bench throughput -- --save-baseline monocoque-v0.1

# Compare against baseline
cargo bench --bench throughput -- --baseline monocoque-v0.1
```

### System Configuration

For stable, high-performance measurements:

```bash
# Disable CPU frequency scaling (Linux)
sudo cpupower frequency-set --governor performance

# Set high priority for benchmarks
sudo nice -n -20 cargo bench

# Pin to specific CPU cores
taskset -c 0,1 cargo bench --bench latency
```

## Understanding Results

### Output Format

Criterion generates:

-   **HTML reports**: `target/criterion/report/index.html`
-   **CSV data**: `target/criterion/<benchmark>/*/estimates.json`
-   **Console output**: Summary statistics

### Key Metrics

**Throughput**:

-   Higher is better
-   Look for: msgs/sec, MB/sec
-   Compare: monocoque vs rust-zmq ratio

**Latency**:

-   Lower is better
-   Look for: mean, median, p95, p99
-   Target: Sub-10μs for local connections

**Patterns**:

-   Scaling characteristics
-   Look for: Linear vs sub-linear scaling
-   Verify: Optimization effectiveness

### Interpreting Results

```
throughput/monocoque/req_rep/256B
                        time:   [450.23 μs 452.67 μs 455.34 μs]
                        thrpt:  [2.21 M msg/s]

throughput/rust_zmq/req_rep/256B
                        time:   [850.45 μs 853.12 μs 856.03 μs]
                        thrpt:  [1.17 M msg/s]
```

**Analysis**: Monocoque is ~1.9x faster (2.21M vs 1.17M msg/s)

## Benchmark Details

### Message Sizes

-   **Small**: 8-64 bytes (typical control messages)
-   **Medium**: 256-1024 bytes (typical data messages)
-   **Large**: 4-16KB (bulk transfers)

### Test Configurations

**REQ/REP**:

-   Synchronous request/response
-   Measures round-trip latency
-   Single client ↔ server

**DEALER/ROUTER**:

-   Asynchronous messaging
-   Load balancing potential
-   Identity-based routing

**PUB/SUB**:

-   One-to-many broadcast
-   Topic-based filtering
-   Fanout scaling (1, 5, 10, 20, 50 subscribers)

## Performance Targets

Based on blueprint specifications:

| Metric             | Target     | Measured | Status |
| ------------------ | ---------- | -------- | ------ |
| Throughput (small) | > 1M msg/s | TBD      | ⏳     |
| Throughput (large) | > 500 MB/s | TBD      | ⏳     |
| Latency (local)    | < 10μs     | TBD      | ⏳     |
| PUB/SUB fanout     | Linear     | TBD      | ⏳     |

## Optimizations Benchmarked

### Zero-Copy

-   **Implementation**: All payloads use `Bytes` (refcounted)
-   **Benefit**: No memcpy for message routing
-   **Measure**: Compare same-size message throughput

### io_uring

-   **Implementation**: Compio runtime with io_uring backend
-   **Benefit**: Reduced syscalls, batched I/O
-   **Measure**: Latency reduction vs blocking I/O

### Arena Allocation

-   **Implementation**: 8KB slabs for read buffers
-   **Benefit**: Reduced allocator pressure
-   **Measure**: Throughput stability under load

### Sorted Subscription Index

-   **Implementation**: Sorted `Vec<Bytes>` with early exit
-   **Benefit**: Cache-friendly, predictable branches
-   **Measure**: Topic filtering overhead

### SmallVec Multipart

-   **Implementation**: Inline storage for 1-4 frames
-   **Benefit**: Zero heap allocations for typical messages
-   **Measure**: Multipart message throughput

## Comparison Matrix

| Feature      | Monocoque         | rust-zmq | Advantage |
| ------------ | ----------------- | -------- | --------- |
| I/O Model    | io_uring          | epoll    | Monocoque |
| Memory       | Zero-copy (Bytes) | Memcpy   | Monocoque |
| Allocation   | Arena             | General  | Monocoque |
| Runtime      | Async (compio)    | Blocking | Monocoque |
| Subscription | Sorted Vec        | HashMap  | Monocoque |
| API          | Rust-native       | C FFI    | Monocoque |

## Contributing Benchmarks

### Adding New Benchmarks

1. Create benchmark in `benches/`
2. Add to `Cargo.toml` `[[bench]]` section
3. Use criterion for statistical rigor
4. Document expected performance characteristics

### Benchmark Guidelines

-   **Warmup**: Run 100+ iterations before measuring
-   **Duration**: 10-15 seconds for stable results
-   **Samples**: 30-200 samples per benchmark
-   **Isolation**: Avoid cross-benchmark interference
-   **Repeatability**: Use fixed seeds, controlled environment

## CI/CD Integration

Benchmarks run automatically on:

-   Major commits to `main`
-   Pull requests (performance regression check)
-   Release tags (publish results)

Results stored in: `bench_results/<commit>/`

## References

-   [Criterion.rs Documentation](https://bheisler.github.io/criterion.rs/book/)
-   [ZeroMQ Benchmark Methodology](http://zeromq.org/results:perf)
-   [io_uring Performance Analysis](https://kernel.dk/io_uring.pdf)

## License

MIT - Same as Monocoque project
