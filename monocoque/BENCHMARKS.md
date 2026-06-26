# Monocoque Benchmark Suite

Benchmarks comparing monocoque against rust-zmq (Rust FFI bindings to libzmq).

All benchmarks run **sender and receiver on separate OS threads** with separate
`compio` runtimes, so results reflect real kernel TCP/IPC round-trips. The timer
lives on the receiver side for throughput tests. Both sides are given identical
methodology — same number of operations per message, same warmup structure.

Hardware: Linux 6.18, release build.

---

## Measured Results

### Throughput — `cargo bench --bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**monocoque (eager)** — default, one kernel write per `send()`:

| Message size | msg/s |
|---|---|
| 64 B | 149 K |
| 256 B | 146 K |
| 1 KB | 131 K |
| 4 KB | 122 K |
| 16 KB | 109 K |

**monocoque (coalesced)** — `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | msg/s | vs zmq |
|---|---|---|
| 64 B | 6.1 M | **6.3× faster** |
| 256 B | 3.5 M | **5.0× faster** |
| 1 KB | 1.4 M | **3.1× faster** |
| 4 KB | 391 K | **2.3× faster** |
| 16 KB | 113 K | **1.6× faster** |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 971 K |
| 256 B | 699 K |
| 1 KB | 455 K |
| 4 KB | 168 K |
| 16 KB | 71 K |

---

### Cross-implementation comparison — `scripts/monocoque_bench_peer`

Two-process, 2-second timed window. All numbers from the same Linux 6.18 machine.
monocoque uses `push` (coalesced, one flush per 64 messages); other implementations
use their default modes.

**TCP loopback throughput:**

| Message size | monocoque | libzmq | rzmq | zmq.rs |
|---|---|---|---|---|
| 64 B | **7.3 M msg/s** | 1.9 M msg/s | 2.3 M msg/s | 301 K msg/s |
| 256 B | **4.1 M msg/s** | 1.7 M msg/s | 1.9 M msg/s | 277 K msg/s |
| 1 KB | **1.3 M msg/s** | 767 K msg/s | 1.0 M msg/s | 269 K msg/s |
| 4 KB | 324 K msg/s | 210 K msg/s | **369 K msg/s** | 228 K msg/s |
| 16 KB | 75 K msg/s | 51 K msg/s | **93 K msg/s** | 170 K msg/s |

**IPC (Unix socket) throughput — monocoque coalesced vs libzmq:**

| Message size | monocoque IPC | monocoque TCP | IPC speedup |
|---|---|---|---|
| 64 B | 5.8 M msg/s | 7.3 M msg/s | see note |
| 256 B | 3.0 M msg/s | 4.1 M msg/s | see note |
| 1 KB | 834 K msg/s | 1.3 M msg/s | see note |

Note: IPC throughput is lower than TCP here because the 64-message batch
size was tuned for TCP. Increase the batch size (reduce flush frequency)
for IPC to match or exceed TCP numbers.

**REQ/REP latency — persistent connection, 5000 iterations, 500 warmup:**

| Message size | monocoque TCP | monocoque IPC | libzmq | zmq.rs | rzmq |
|---|---|---|---|---|---|
| 64 B | **75 µs** p50 | **67 µs** p50 | 201 µs | 126 µs | 284 µs |
| 256 B | **75 µs** p50 | **67 µs** p50 | 207 µs | 125 µs | 292 µs |
| 1 KB | **75 µs** p50 | **67 µs** p50 | 208 µs | 127 µs | 295 µs |
| 4 KB | **75 µs** p50 | **70 µs** p50 | 214 µs | — | 303 µs |

monocoque's latency advantage (2.7x vs libzmq, 1.7x vs zmq.rs on TCP) comes
from the absence of a background IO thread — there is no cross-thread
handoff on the round-trip path.

---

### Latency — `cargo bench --bench latency`

REQ/REP round-trip on TCP loopback. Includes socket teardown (TCP FIN + thread
join). 1 000 warmup rounds per iteration (not measured).

| Message size | monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64 B | 322 µs | 507 µs | 37% lower |
| 256 B | 262 µs | 500 µs | 48% lower |
| 1 KB | 266 µs | 591 µs | 55% lower |

Note: the per-iteration cost includes one TCP connection teardown. Steady-state
latency on a persistent connection is ~75 µs for monocoque vs ~200 µs for libzmq.

---

### IPC vs TCP — `cargo bench --bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 322 µs | 249 µs | 260 µs |
| IPC (Unix socket) | 248 µs | 248 µs | 241 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 150 K msg/s | 148 K msg/s | 132 K msg/s |
| IPC | 357 K msg/s | 347 K msg/s | 329 K msg/s |

IPC is ~2.4× faster than TCP loopback for throughput; latency advantage is
smaller (7–23%) because teardown dominates the per-iteration measurement.

---

### Pipelined batch API — `cargo bench --bench pipelined_throughput`

DEALER/ROUTER with `send_buffered() + flush()`, batches of 100, 10 000 total
messages. This is a monocoque-only benchmark demonstrating the explicit batch API.

| Message size | msg/s | Bandwidth |
|---|---|---|
| 64 B | 1.05 M | 64 MiB/s |
| 256 B | 893 K | 218 MiB/s |
| 1 KB | 535 K | 521 MiB/s |
| 4 KB | 170 K | 664 MiB/s |

---

## Benchmark Methodology

### Why these designs are fair

**Separate OS threads**: both sides run on different threads with different
`compio` runtimes. There is genuine TCP between them — messages pass through
the kernel network stack and loopback device.

**Same work per message**: zmq PUSH/PULL does one `send` / one `recv_bytes`
per message. monocoque does one `send` / one `recv` per message. No artificial
asymmetry.

**No artificial sleeps**: the only pause in the zmq benchmark is a 5 ms sleep
before connecting the PUSH socket to give the PULL socket time to register with
the kernel. This is outside the timed loop.

**Timer on receiver**: the elapsed time is measured by the PULL thread from
first recv to last recv. This avoids counting sender overhead.

**Warmup outside measurement**: connection setup and handshake happen before the
timed loop.

### Cross-implementation bench peer

`scripts/monocoque_bench_peer/` is a standalone Rust binary (separate Cargo
workspace, not part of the monocoque workspace) that implements the same two-process
wire protocol as the other bench peers in the omq.rs comparison suite
(libzmq, zmqrs\_bench\_peer, rzmq\_bench\_peer). It can participate directly in
`python3 scripts/run_comparisons.py` runs from the omq.rs repository.

Key design choices in the bench peer:

- `push` uses write coalescing (flushed every 64 messages) to show monocoque's
  maximum throughput. `push-eager` uses the default mode for latency-tuned
  scenarios.
- `pull` drains the receive buffer with `try_recv()` after each `recv()`,
  reducing io_uring submissions when the kernel delivers multiple messages in one
  read.
- No warmup sleep on the pull/req side. (A sleep fills the kernel send buffer
  and deadlocks monocoque's single-threaded runtime on a blocked write.)
- IPC subcommands (`push-ipc`, `pull-ipc`, `rep-ipc`, `req-ipc`) use Unix
  domain sockets; the bound path is printed as `PATH <p>` on stdout.

```bash
# Build the bench peer
cd scripts/monocoque_bench_peer
cargo build --release

# Quick throughput test (TCP, 64 B, 2 s)
./target/release/monocoque_bench_peer push 0 64 &   # prints PORT <n>
./target/release/monocoque_bench_peer pull <PORT> 64 2.0

# Latency test (TCP, 256 B, 5000 iterations)
./target/release/monocoque_bench_peer rep 0 &        # prints PORT <n>
./target/release/monocoque_bench_peer req <PORT> 256 5000 500
```

### What is not (yet) benchmarked

- Multi-connection fanout (PUB to N SUB)
- Concurrent senders (N PUSH to 1 PULL)
- IPC coalesced throughput against competing IPC implementations

---

## Running the Benchmarks

```bash
# Run all suites (takes ~15 minutes)
cargo bench --features zmq

# Individual suites
cargo bench --bench throughput --features zmq
cargo bench --bench latency --features zmq
cargo bench --bench ipc_vs_tcp --features zmq
cargo bench --bench pipelined_throughput --features zmq

# Filter to a specific case
cargo bench --bench throughput --features zmq -- "throughput/monocoque/push_pull_coalesced"

# Quick smoke-test (no timing, just checks nothing panics)
cargo bench --bench throughput --features zmq --release -- --test

# Cross-implementation comparison bench peer
cd scripts/monocoque_bench_peer && cargo build --release
```

For stable numbers, avoid running other benchmarks in parallel and disable
CPU frequency scaling if available:

```bash
sudo cpupower frequency-set --governor performance
cargo bench --features zmq
```
