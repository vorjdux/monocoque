# Monocoque Benchmark Suite

Benchmarks comparing monocoque against rust-zmq (Rust FFI bindings to libzmq).

All benchmarks run **sender and receiver on separate OS threads** with separate
`compio` runtimes, so results reflect real kernel TCP/IPC round-trips. The timer
lives on the receiver side for throughput tests. Both sides are given identical
methodology — same number of operations per message, same warmup structure.

Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build.

---

## Measured Results

### Throughput — `cargo bench --bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**monocoque (eager)** — default, one kernel write per `send()`:

| Message size | msg/s |
|---|---|
| 64 B | 339 K |
| 256 B | 343 K |
| 1 KB | 314 K |
| 4 KB | 282 K |
| 16 KB | 252 K |

**monocoque (coalesced)** — `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | msg/s | vs zmq |
|---|---|---|
| 64 B | 9.2 M | **7.0× faster** |
| 256 B | 5.5 M | **5.1× faster** |
| 1 KB | 2.3 M | **3.5× faster** |
| 4 KB | 857 K | **2.7× faster** |
| 16 KB | 265 K | **2.4× faster** |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 1.32 M |
| 256 B | 1.08 M |
| 1 KB | 667 K |
| 4 KB | 314 K |
| 16 KB | 111 K |

---

### Cross-implementation comparison — `scripts/monocoque_bench_peer`

> Provenance: this section is from a separate prior host (Linux 6.18), not the
> i7-1355U reference used for the criterion tables above. It was not re-run in
> the latest pass: the multi-implementation columns (libzmq, rzmq, zmq.rs)
> require the external omq.rs comparison harness. Treat these as relative,
> cross-implementation shape rather than absolute numbers for the reference
> machine. The monocoque-vs-libzmq comparison on the reference machine is the
> `throughput` and `latency` criterion tables above.

Two-process, 2-second timed window.
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
| 64 B | 63 µs | 304 µs | 79% lower |
| 256 B | 72 µs | 296 µs | 76% lower |
| 1 KB | 72 µs | 312 µs | 77% lower |

Note: the per-iteration cost includes one TCP connection teardown. Steady-state
latency on a persistent connection is ~75 µs for monocoque vs ~200 µs for libzmq.

---

### IPC vs TCP — `cargo bench --bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 77 µs | 82 µs | 78 µs |
| IPC (Unix socket) | 83 µs | 84 µs | 92 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 340 K msg/s | 341 K msg/s | 309 K msg/s |
| IPC | 716 K msg/s | 702 K msg/s | 662 K msg/s |

IPC is ~2.1× faster than TCP loopback for throughput. On this run IPC and TCP
latency land within each other's noise band because per-iteration teardown
dominates the measurement, so the IPC advantage shows up on throughput, not
latency.

---

### Pipelined batch API — `cargo bench --bench pipelined_throughput`

DEALER/ROUTER with `send_buffered() + flush()`, batches of 100, 10 000 total
messages. This is a monocoque-only benchmark demonstrating the explicit batch API.

| Message size | msg/s | Bandwidth |
|---|---|---|
| 64 B | 2.45 M | 150 MiB/s |
| 256 B | 1.98 M | 484 MiB/s |
| 1 KB | 1.08 M | 1.03 GiB/s |
| 4 KB | 387 K | 1.48 GiB/s |

---

### Vectored writes, recv_batch, PUB coalescing

These paths have their own focused harness (not part of the criterion suite):
`monocoque/examples/bench_changes.rs`, run with
`cargo run --release --features zmq --example bench_changes`. It toggles each
change via its public knob so the effect is isolated. Numbers below are a
separate loopback run; treat them as relative (they show the direction of each
change), not directly comparable to the criterion tables above.

**Vectored writes (PUSH/PULL eager, one message per `send`)**, copy vs
`writev`, by frame size:

| Frame size | copy | vectored | ratio |
|---|---|---|---|
| 16 KB | 1.86 GB/s | 1.28 GB/s | 0.69x |
| 32 KB | 1.65 GB/s | 2.10 GB/s | 1.27x |
| 64 KB | 1.33 GB/s | 1.68 GB/s | 1.26x |
| 256 KB | 1.82 GB/s | 2.22 GB/s | 1.22x |
| 1 MB | 1.24 GB/s | 1.48 GB/s | 1.19x |

The crossover is ~32 KB (hence the default `vectored_write_threshold`); below it
the contiguous copy plus one `write` beats a two-segment `writev`.

**`recv_batch` vs `recv`** (64 B, `send_batch(256)`): 6.1 M vs 7.8 M msg/s, no
win on loopback; kept as an ergonomic API.

**PUB→SUB delivered broadcast, 1 subscriber** (coalescing on): 64 B ~174 K
msg/s, 1 KB ~161 K msg/s.

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

- PUB fan-out to **many** subscribers (single-subscriber delivered throughput is
  measured above; the coalescing path is designed to amortize syscalls across
  subscribers under load, which still needs an N-SUB benchmark)
- Concurrent senders (N PUSH to 1 PULL)
- IPC coalesced throughput against competing IPC implementations
- A clean on/off A/B for PUB coalescing (the cap is a compile-time constant)

---

## Running the Benchmarks

All commands below work from either the workspace root or the `monocoque/`
subdirectory. Use `-p monocoque` when running from the workspace root to
avoid also running the allocator micro-benchmarks (`allocation` bench has no
`required-features`, so `cargo bench` without `-p` picks it up separately).

```bash
# Run the comparison suites (throughput, latency, IPC, pipelined, patterns)
# Takes ~20 minutes; add -p monocoque if running from the workspace root.
cargo bench -p monocoque --features zmq \
    --bench throughput --bench latency --bench ipc_vs_tcp \
    --bench pipelined_throughput --bench patterns

# Run the allocator micro-benchmarks (no zmq dependency)
cargo bench -p monocoque --bench allocation

# Individual comparison suite
cargo bench -p monocoque --bench throughput --features zmq
cargo bench -p monocoque --bench latency --features zmq
cargo bench -p monocoque --bench ipc_vs_tcp --features zmq
cargo bench -p monocoque --bench pipelined_throughput --features zmq
cargo bench -p monocoque --bench patterns --features zmq

# Filter to a specific case
cargo bench -p monocoque --bench throughput --features zmq -- "throughput/monocoque/push_pull_coalesced"

# Quick smoke-test (no timing, just checks nothing panics)
cargo bench -p monocoque --bench throughput --features zmq -- --test

# Cross-implementation comparison bench peer
cd scripts/monocoque_bench_peer && cargo build --release
```

For stable numbers, avoid running other benchmarks in parallel and disable
CPU frequency scaling if available:

```bash
sudo cpupower frequency-set --governor performance
cargo bench -p monocoque --features zmq \
    --bench throughput --bench latency --bench ipc_vs_tcp \
    --bench pipelined_throughput --bench patterns
```
