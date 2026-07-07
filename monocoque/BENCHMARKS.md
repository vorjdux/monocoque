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
counts, which these benches do not cover. The compio and tokio throughput figures
are the established measurements; the rust-zmq throughput column was re-measured
with a corrected live-connection timer; smol was added from a fresh run on the
same scale; and the latency table is a fresh steady-state run for all backends.

Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build.

---

## Measured Results

### Throughput - `cargo bench --bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**eager** - default, one kernel write per `send()`:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 339 K | 520 K | 412 K |
| 256 B | 344 K | 514 K | 403 K |
| 1 KB | 318 K | 410 K | 383 K |
| 4 KB | 292 K | 417 K | 346 K |
| 16 KB | 266 K | 317 K | 282 K |

**coalesced** - `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 9.2 M | **13.6 M** | 10.1 M |
| 256 B | 5.6 M | **9.8 M** | 6.9 M |
| 1 KB | 2.4 M | **5.3 M** | 3.0 M |
| 4 KB | 841 K | **1.74 M** | 1.05 M |
| 16 KB | 268 K | **473 K** | 342 K |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 4.73 M |
| 256 B | 2.66 M |
| 1 KB | 1.04 M |
| 4 KB | 394 K |
| 16 KB | 120 K |

Eager mode is a latency tool (each `send()` goes out immediately, one syscall per
message), not a throughput one. On a bulk one-way firehose, libzmq's internal
IO-thread batching wins small messages (4.73 M vs 339-520 K at 64 B, ~9-14x); the
gap closes with size, reaching near parity at 4 KB and monocoque leading ~2.2-2.6x
at 16 KB where vectored writes avoid the copy. With write coalescing, all three
backends beat libzmq by ~2-4x across the range (at 64 B ~1.9x compio, ~2.9x tokio,
~2.1x smol); smol lands between compio and tokio. Reach for eager when per-message
delivery latency matters; turn on coalescing for small-message throughput.

The rust-zmq column is measured with the receiver timer starting on a live
connection (one warmup message before the clock), the same as the monocoque path;
an earlier version started the zmq timer before the sender connected, which
understated libzmq at small sizes.

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

REQ/REP steady-state round-trip on TCP loopback. The connection is established
once (plus 200 warmup rounds) outside the timer, then N back-to-back round-trips
are timed; socket teardown and thread join happen after the clock stops.

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 10.6 µs | 10.2 µs | 13.1 µs | 34.1 µs |
| 256 B | 10.1 µs | 9.6 µs | 13.0 µs | 35.4 µs |
| 1 KB | 10.5 µs | 9.7 µs | 12.3 µs | 35.1 µs |

All three backends are ~2.7-3.5x lower round-trip latency than libzmq's ~35 µs
(tokio ~9.8 µs, compio ~10.5 µs, smol ~12.8 µs). The advantage comes from doing
the I/O inline on one thread, with no handoff to a background IO thread the way
libzmq does. tokio edges compio because an epoll wakeup for a single-flow
round-trip is a touch shorter than submitting and reaping an io_uring completion.

---

### IPC vs TCP - `cargo bench --bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 66 µs | 61 µs | 61 µs |
| compio IPC | 67 µs | 68 µs | 70 µs |
| tokio TCP | 45 µs | 47 µs | 51 µs |
| tokio IPC | 59 µs | 57 µs | 56 µs |
| smol TCP | 88 µs | 85 µs | 87 µs |
| smol IPC | 80 µs | 79 µs | 80 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 351 K msg/s | 347 K msg/s | 322 K msg/s |
| compio IPC | 735 K msg/s | 717 K msg/s | 683 K msg/s |
| tokio TCP | 518 K msg/s | 513 K msg/s | 486 K msg/s |
| tokio IPC | 1.54 M msg/s | 1.47 M msg/s | 1.54 M msg/s |
| smol TCP | 443 K msg/s | 413 K msg/s | 233 K msg/s |
| smol IPC | 1.49 M msg/s | 1.34 M msg/s | 1.19 M msg/s |

IPC is ~2.1x (compio) to ~3.4x (smol) faster than TCP loopback for throughput. On
all three backends IPC and TCP latency land within each other's noise band because
per-iteration teardown dominates the measurement, so the IPC advantage shows up
on throughput, not latency.

---

### Pipelined batch API - `cargo bench --bench pipelined_throughput`

DEALER/ROUTER with `send_buffered() + flush()`, batches of 100, 10 000 total
messages. This is a monocoque-only benchmark demonstrating the explicit batch API.

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 2.49 M (152 MiB/s) | 2.70 M (165 MiB/s) | 2.02 M (123 MiB/s) |
| 256 B | 1.98 M (482 MiB/s) | 2.22 M (542 MiB/s) | 1.69 M (412 MiB/s) |
| 1 KB | 1.13 M (1.08 GiB/s) | 1.52 M (1.45 GiB/s) | 1.14 M (1.09 GiB/s) |
| 4 KB | 341 K (1.30 GiB/s) | 582 K (2.22 GiB/s) | 412 K (1.57 GiB/s) |
| 16 KB | 87 K (1.32 GiB/s) | 105 K (1.60 GiB/s) | 96 K (1.46 GiB/s) |

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
| 64 B | 12.8 M | 12.5 M | 11.9 M |
| 1 KB | 2.67 M | 2.97 M | 2.91 M |
| 16 KB | 277 K | 271 K | 325 K |

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
coalesced 64 B sink stays around 11 M msg/s.

Fan-in, coalescing senders (large kernel-read batches):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 11.5 M | 15.9 M | 10.7 M |
| 1 KB | 2.14 M | 2.92 M | 2.69 M |
| 16 KB | 256 K | 306 K | 302 K |

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
