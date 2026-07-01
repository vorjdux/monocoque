# Monocoque Benchmark Suite

Benchmarks comparing monocoque against rust-zmq (Rust FFI bindings to libzmq).

All benchmarks run **sender and receiver on separate OS threads** with separate
runtimes, so results reflect real kernel TCP/IPC round-trips. The timer lives on
the receiver side for throughput tests. Both sides are given identical
methodology - same number of operations per message, same warmup structure.

Both runtime backends run the identical suite (compio uses io_uring, tokio uses
epoll). On these single-flow loopback microbenchmarks the tokio/epoll backend is
consistently a bit faster: a one-connection ping-pong does not exercise io_uring's
strengths (batched submission, registered buffers, many concurrent connections)
and just pays its per-op submission overhead. compio (io_uring) is the default;
its edge is on real network I/O and high connection counts, which these benches
do not cover. The rust-zmq control was stable across both backend runs, so the
compio-vs-tokio gap is a real measurement, not machine drift.

Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build.

---

## Measured Results

### Throughput - `cargo bench --bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**eager** - default, one kernel write per `send()`:

| Message size | compio | tokio |
|---|---|---|
| 64 B | 339 K | 520 K |
| 256 B | 344 K | 514 K |
| 1 KB | 318 K | 410 K |
| 4 KB | 292 K | 417 K |
| 16 KB | 266 K | 317 K |

**coalesced** - `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | compio | tokio |
|---|---|---|
| 64 B | 9.2 M | **13.6 M** |
| 256 B | 5.6 M | **9.8 M** |
| 1 KB | 2.4 M | **5.3 M** |
| 4 KB | 841 K | **1.74 M** |
| 16 KB | 268 K | **473 K** |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 1.33 M |
| 256 B | 1.09 M |
| 1 KB | 656 K |
| 4 KB | 328 K |
| 16 KB | 117 K |

Coalesced, both backends beat libzmq by a wide margin: ~7x (compio) to ~10x
(tokio) at 64 B, tapering to ~2.3x and ~4.0x at 16 KB. In eager mode both trail
libzmq, which amortizes its syscall over an internal IO-thread batch.

The PULL side allocates a `Vec<Bytes>` per message by default. Receiving into a
reused buffer with `recv_into` removes that allocation; the
`push_pull_coalesced_recv_into` bench case shows ~1.23x at 64 B (9.2 M to 11.3 M
on compio, 13.6 M to 15.7 M on tokio) and ~13% at 256 B over the `recv()` path
(the gain tapers as messages grow and the path becomes bandwidth-bound). See
`docs/performance.md` for details.

---

### Cross-implementation comparison - `scripts/monocoque_bench_peer`

> Provenance: this section is from a separate prior host (Linux 6.18), not the
> i7-1355U reference used for the criterion tables above. It was not re-run in
> the latest pass: the multi-implementation columns (libzmq, rzmq, zmq.rs)
> require the external cross-implementation comparison harness. Treat these as relative,
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

**IPC (Unix socket) throughput - monocoque coalesced vs libzmq:**

| Message size | monocoque IPC | monocoque TCP | IPC speedup |
|---|---|---|---|
| 64 B | 5.8 M msg/s | 7.3 M msg/s | see note |
| 256 B | 3.0 M msg/s | 4.1 M msg/s | see note |
| 1 KB | 834 K msg/s | 1.3 M msg/s | see note |

Note: IPC throughput is lower than TCP here because the 64-message batch
size was tuned for TCP. Increase the batch size (reduce flush frequency)
for IPC to match or exceed TCP numbers.

**REQ/REP latency - persistent connection, 5000 iterations, 500 warmup:**

| Message size | monocoque TCP | monocoque IPC | libzmq | zmq.rs | rzmq |
|---|---|---|---|---|---|
| 64 B | **75 µs** p50 | **67 µs** p50 | 201 µs | 126 µs | 284 µs |
| 256 B | **75 µs** p50 | **67 µs** p50 | 207 µs | 125 µs | 292 µs |
| 1 KB | **75 µs** p50 | **67 µs** p50 | 208 µs | 127 µs | 295 µs |
| 4 KB | **75 µs** p50 | **70 µs** p50 | 214 µs | - | 303 µs |

monocoque's latency advantage (2.7x vs libzmq, 1.7x vs zmq.rs on TCP) comes
from the absence of a background IO thread - there is no cross-thread
handoff on the round-trip path.

---

### Latency - `cargo bench --bench latency`

REQ/REP round-trip on TCP loopback. Includes socket teardown (TCP FIN + thread
join). 1 000 warmup rounds per iteration (not measured).

| Message size | compio | tokio | rust-zmq |
|---|---|---|---|
| 64 B | 58 µs | 43 µs | 277 µs |
| 256 B | 51 µs | 42 µs | 265 µs |
| 1 KB | 61 µs | 48 µs | 261 µs |

Both backends are ~5x lower latency than libzmq (79% lower for compio, 84% for
tokio at 64 B); tokio edges compio because the epoll wakeup for a single-flow
round-trip is shorter than submitting and reaping an io_uring completion. The
per-iteration cost includes one TCP connection teardown, so steady-state latency
on a persistent connection is lower than these figures for both.

---

### IPC vs TCP - `cargo bench --bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 59 µs | 51 µs | 56 µs |
| compio IPC | 57 µs | 67 µs | 60 µs |
| tokio TCP | 42 µs | 45 µs | 44 µs |
| tokio IPC | 55 µs | 54 µs | 57 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 349 K msg/s | 346 K msg/s | 315 K msg/s |
| compio IPC | 724 K msg/s | 710 K msg/s | 683 K msg/s |
| tokio TCP | 520 K msg/s | 508 K msg/s | 463 K msg/s |
| tokio IPC | 1.47 M msg/s | 1.64 M msg/s | 1.39 M msg/s |

IPC is ~2.1× (compio) to ~3× (tokio) faster than TCP loopback for throughput. On
both backends IPC and TCP latency land within each other's noise band because
per-iteration teardown dominates the measurement, so the IPC advantage shows up
on throughput, not latency.

---

### Pipelined batch API - `cargo bench --bench pipelined_throughput`

DEALER/ROUTER with `send_buffered() + flush()`, batches of 100, 10 000 total
messages. This is a monocoque-only benchmark demonstrating the explicit batch API.

| Message size | compio | tokio |
|---|---|---|
| 64 B | 2.61 M (159 MiB/s) | 2.77 M (169 MiB/s) |
| 256 B | 2.04 M (497 MiB/s) | 2.14 M (522 MiB/s) |
| 1 KB | 1.14 M (1.09 GiB/s) | 1.56 M (1.49 GiB/s) |
| 4 KB | 372 K (1.42 GiB/s) | 619 K (2.36 GiB/s) |
| 16 KB | 97 K (1.49 GiB/s) | 118 K (1.80 GiB/s) |

---

### Fan-out / fan-in worker pools - `cargo bench --bench fanout_fanin`

Monocoque-only throughput for the two pool topologies, `WORKERS = 4`, 10 000
messages per iteration split evenly across the pool. `fanout` is one `PushFanOut`
ventilator round-robining to four PULL workers (timed across the workers, so the
cost is when the last message lands); `fanin` is four PUSH workers merged by one
`PullFanIn` sink (timed on the sink). This is the in-process counterpart to the
bench peer's `push-fanout` / `pull-fanin` subcommands. The msg/s figure is
aggregate delivered throughput across the pool; bandwidth is the matching payload
rate.

Fan-out (one ventilator, four PULL workers), coalescing senders (msg/s;
bandwidth is msg/s x frame size):

| Message size | compio | tokio |
|---|---|---|
| 64 B | 12.7 M | 12.2 M |
| 1 KB | 2.56 M | 2.89 M |
| 16 KB | 259 K | 260 K |

The ventilator round-robins one message at a time; with coalescing each worker's
buffer flushes at the 64 KB threshold, so the writes stay batched while the four
workers receive interleaved and in parallel. Handing each worker its whole share
in one batched write instead serializes the pool (worker 1 waits for worker 0's
entire share) and is markedly slower at large messages, so the per-message path
is the one kept.

Fan-in merges four sources into one sink. The sink's bottleneck is the per-message
cross-task hop into its merge channel plus one `.await` per message, all on one
runtime. `PullFanIn` removes most of that by batching: each reader forwards a whole
kernel-read batch as one channel item and the sink drains a local buffer, so
`recv_batch` pays one channel hop and one `.await` per batch instead of per message.

Fan-in, coalescing senders (large kernel-read batches):

| Message size | compio | tokio |
|---|---|---|
| 64 B | 10.9 M | 11.5 M |
| 1 KB | 2.12 M | 2.55 M |
| 16 KB | 259 K | 273 K |

The reader-side batching keeps the coalesced 64 B sink around 11 M msg/s; at larger
sizes the path is bandwidth-bound, so the sender mode matters less.

Fan-in, eager senders (one write per message):

| Message size | compio | tokio |
|---|---|---|
| 64 B | 567 K | 1.12 M |
| 1 KB | ~520 K | ~110 K |
| 16 KB | 229 K | 262 K |

With eager senders the four PUSH workers cap throughput at their per-message write
rate, well below what the sink can drain, so the sink is no longer the bottleneck
and the same batched path neither helps nor hurts. The batch size simply follows
what each kernel read delivers, so there is one code path for both sender modes.
The 1 KB eager row is noisy run to run (the workers and sink trade the
bottleneck), so treat it as approximate.

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
`compio` runtimes. There is genuine TCP between them - messages pass through
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
wire protocol as the other bench peers in the cross-implementation comparison suite
(libzmq, zmqrs\_bench\_peer, rzmq\_bench\_peer). It can participate directly in
the external comparison harness that drives those peers side by side.

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
- Fan-out / fan-in subcommands drive the worker-pool topologies: `push-fanout`
  binds a ventilator that round-robins to N PULL workers, `pull-fanin` binds a
  sink that merges N PUSH workers, and `push-connect` is the connecting PUSH used
  as a fan-in worker. Fan-out workers reuse the plain `pull` subcommand.

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

# Fan-in: 3 PUSH workers into 1 sink (TCP, 64 B, 2 s)
./target/release/monocoque_bench_peer pull-fanin 0 64 2.0 3 &  # prints PORT <n>
./target/release/monocoque_bench_peer push-connect <PORT> 64 & # repeat per worker
./target/release/monocoque_bench_peer push-connect <PORT> 64 &
./target/release/monocoque_bench_peer push-connect <PORT> 64 &

# Fan-out: 1 ventilator round-robins to 3 PULL workers (TCP, 64 B, 2 s)
./target/release/monocoque_bench_peer push-fanout 0 64 3 &      # prints PORT <n>
./target/release/monocoque_bench_peer pull <PORT> 64 2.0 &      # repeat per worker
./target/release/monocoque_bench_peer pull <PORT> 64 2.0 &
./target/release/monocoque_bench_peer pull <PORT> 64 2.0
```

### What is not (yet) benchmarked

- PUB fan-out to **many** subscribers (single-subscriber delivered throughput is
  measured above; the coalescing path is designed to amortize syscalls across
  subscribers under load, which still needs an N-SUB benchmark)
- Fan-out / fan-in worker pools **against other implementations**: the
  in-process `fanout_fanin` criterion bench covers monocoque, and the
  `push-fanout` / `pull-fanin` bench-peer subcommands exist, but a measured
  cross-implementation comparison has not been collected yet
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
cargo bench -p monocoque --bench fanout_fanin --features zmq

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
