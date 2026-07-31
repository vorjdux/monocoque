# Monocoque Benchmark Suite

Benchmarks comparing monocoque against rust-zmq (Rust FFI bindings to libzmq).

All benchmarks run **sender and receiver on separate OS threads** with separate
runtimes, so results reflect real kernel TCP/IPC round-trips. The timer lives on
the receiver side for throughput tests. Both sides are given identical
methodology - same number of operations per message, same warmup structure.

The three runtime backends run the identical suite (compio uses io_uring, tokio
and smol use epoll). On these single-flow loopback microbenchmarks the epoll
backends (tokio, smol) are consistently a bit faster: a one-connection ping-pong
does not exercise io_uring's strengths (batched submission, registered buffers,
many concurrent connections) and just pays its per-op submission overhead. compio
(io_uring) is the default; its edge is on real network I/O and high connection
counts, which these benches do not cover. The criterion tables below (throughput,
latency, IPC-vs-TCP) were all re-measured together for the 0.4.0 release on a
quiet machine, on the same corrected live-connection timer. The cross-process
multi-implementation comparison tables further down come from a separate harness
and are kept for shape, not as absolute reference-machine numbers;
[docs/performance.md](../docs/performance.md) is the canonical breakdown.

Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build, `rustc 1.96`.

---

## Measured Results

### Throughput - `cargo bench --bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**eager** - default, one kernel write per `send()`:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 481 K | 520 K | 437 K |
| 256 B | 481 K | 513 K | 436 K |
| 1 KB | 440 K | 479 K | 396 K |
| 4 KB | 404 K | 419 K | 377 K |
| 16 KB | 335 K | 300 K | 279 K |

**coalesced** - `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 14.1 M | **18.0 M** | 12.9 M |
| 256 B | 8.3 M | **12.6 M** | 8.5 M |
| 1 KB | 3.5 M | **5.7 M** | 3.3 M |
| 4 KB | 1.15 M | **1.70 M** | 1.15 M |
| 16 KB | 356 K | **454 K** | 368 K |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 4.67 M |
| 256 B | 2.66 M |
| 1 KB | 1.06 M |
| 4 KB | 406 K |
| 16 KB | 126 K |

Eager mode is a latency tool (each `send()` goes out immediately, one syscall per
message), not a throughput one. On a bulk one-way firehose, libzmq's internal
IO-thread batching wins small messages (4.67 M vs 437-520 K at 64 B, ~9-11x); the
gap closes with size, reaching near parity at 4 KB and monocoque leading ~2.2-2.7x
at 16 KB where vectored writes avoid the copy. With write coalescing, all three
backends beat libzmq by ~2-4x across the range (at 64 B ~3.0x compio, ~3.9x tokio,
~2.8x smol); tokio leads on these single-flow runs. Reach for eager when per-message
delivery latency matters; turn on coalescing for small-message throughput.

The rust-zmq column is measured with the receiver timer starting on a live
connection (one warmup message before the clock), the same as the monocoque path;
an earlier version started the zmq timer before the sender connected, which
understated libzmq at small sizes.

The PULL side allocates a `Vec<Bytes>` per message by default. Receiving into a
reused buffer with `recv_into` removes that allocation; the
`push_pull_coalesced_recv_into` bench case shows ~1.10x at 64 B (14.1 M to 15.6 M
on compio, 18.0 M to 19.1 M on tokio) and ~9% at 256 B over the `recv()` path
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

REQ/REP steady-state round-trip on TCP loopback. The connection is established
once (plus 200 warmup rounds) outside the timer, then N back-to-back round-trips
are timed; socket teardown and thread join happen after the clock stops.

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 8.4 µs | 9.8 µs | 12.4 µs | 33.8 µs |
| 256 B | 8.5 µs | 9.5 µs | 12.6 µs | 33.3 µs |
| 1 KB | 8.7 µs | 9.7 µs | 13.5 µs | 34.4 µs |

All three backends are ~2.5-4x lower round-trip latency than libzmq's ~34 µs
(compio ~8.5 µs, tokio ~9.7 µs, smol ~12.8 µs). The advantage comes from doing
the I/O inline on one thread, with no handoff to a background IO thread the way
libzmq does. compio edges tokio here: after the 0.19 upgrade, submitting and
reaping an io_uring completion on a single-flow round-trip is at par with or
just under an epoll wakeup.

---

### IPC vs TCP - `cargo bench --bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 51 µs | 53 µs | 56 µs |
| compio IPC | 96 µs | 108 µs | 103 µs |
| tokio TCP | 47 µs | 52 µs | 56 µs |
| tokio IPC | 59 µs | 69 µs | 61 µs |
| smol TCP | 81 µs | 82 µs | 92 µs |
| smol IPC | 83 µs | 83 µs | 90 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 488 K msg/s | 489 K msg/s | 458 K msg/s |
| compio IPC | 1.51 M msg/s | 1.42 M msg/s | 1.30 M msg/s |
| tokio TCP | 513 K msg/s | 523 K msg/s | 473 K msg/s |
| tokio IPC | 1.53 M msg/s | 1.46 M msg/s | 1.37 M msg/s |
| smol TCP | 438 K msg/s | 437 K msg/s | 401 K msg/s |
| smol IPC | 1.44 M msg/s | 1.32 M msg/s | 1.18 M msg/s |

IPC is ~3x faster than TCP loopback for throughput on every backend (~3.1x
compio, ~3.0x tokio, ~3.3x smol), because Unix sockets have lower per-syscall
overhead and no TCP framing. On the latency axis (above), tokio and smol IPC land
within their TCP noise band, while compio IPC runs higher than compio TCP: the
fixed io_uring per-op submit/reap cost dominates the very short Unix-socket round
trip. That op cost is amortized away here on throughput, where compio IPC leads.

---

### Pipelined batch API - `cargo bench --bench pipelined_throughput`

DEALER/ROUTER with `send_buffered() + flush()`, batches of 100, 10 000 total
messages. This is a monocoque-only benchmark demonstrating the explicit batch API.

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 3.34 M (204 MiB/s) | 3.12 M (190 MiB/s) | 2.31 M (141 MiB/s) |
| 256 B | 2.72 M (665 MiB/s) | 2.62 M (639 MiB/s) | 1.94 M (475 MiB/s) |
| 1 KB | 1.71 M (1.63 GiB/s) | 1.66 M (1.58 GiB/s) | 1.40 M (1.34 GiB/s) |
| 4 KB | 713 K (2.72 GiB/s) | 707 K (2.70 GiB/s) | 647 K (2.47 GiB/s) |
| 16 KB | 146 K (2.23 GiB/s) | 157 K (2.39 GiB/s) | 159 K (2.42 GiB/s) |

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

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 16.0 M | 12.9 M | 12.1 M |
| 1 KB | 3.65 M | 3.28 M | 3.20 M |
| 16 KB | 395 K | 352 K | 356 K |

The ventilator round-robins one message at a time; with coalescing each worker's
buffer flushes at the 64 KB threshold, so the writes stay batched while the four
workers receive interleaved and in parallel. Handing each worker its whole share
in one batched write instead serializes the pool (worker 1 waits for worker 0's
entire share) and is markedly slower at large messages, so the per-message path
is the one kept.

Fan-in merges four sources into one sink. The sink's bottleneck is the per-message
cross-task hop into its merge channel plus one `.await` per message, all on one
runtime. `PullFanIn` removes most of that by batching: each reader forwards its
kernel-read batch in bounded-size chunks and the sink drains a local buffer, so
`recv_batch` pays about one channel hop and one `.await` per chunk instead of per
message. The per-chunk cap also bounds how many messages (and the 64 KB slab pages
they pin) can queue while the sink lags its readers, so peak memory stays flat
instead of growing with worker count. Throughput is unchanged by the cap: the
coalesced 64 B sink stays around 10 M msg/s (14 M on tokio).

Fan-in, coalescing senders (large kernel-read batches):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 9.5 M | 14.0 M | 10.0 M |
| 1 KB | 2.83 M | 2.99 M | 2.88 M |
| 16 KB | 317 K | 310 K | 291 K |

The reader-side batching keeps the coalesced 64 B sink around 11 M msg/s; at larger
sizes the path is bandwidth-bound, so the sender mode matters less.

Fan-in, eager senders (one write per message):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 634 K | 1.18 M | 1.46 M |
| 1 KB | ~548 K | ~28 K | ~30 K |
| 16 KB | 263 K | 290 K | 279 K |

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

**No setup in the timed window**: the zmq benchmark uses a 5 ms sleep before
connecting the PUSH socket so the PULL socket registers with the kernel first, but
the PULL receives one warmup message before starting its timer, so that sleep and
the connection setup fall outside the measured window. This matches the monocoque
path, which starts its timer only after `accept()` and the ZMTP handshake. (An
earlier version started the zmq timer before the PUSH connected, folding connect
plus the 5 ms into the measurement and understating libzmq at small sizes; that
is fixed.)

**Timer on receiver, on a live connection**: elapsed time is measured by the PULL
thread from the first steady-state recv to the last, so no sender overhead or
connection setup is counted on either side.

**Warmup and teardown outside measurement**: connection setup and handshake happen
before the timer on both sides; the latency bench additionally runs on a
persistent connection with socket teardown after the timer, so it reports
steady-state round-trip time.

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
