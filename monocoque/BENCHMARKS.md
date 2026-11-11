# Monocoque Benchmark Suite

Benchmarks comparing monocoque against rust-zmq (Rust FFI bindings to libzmq).

All benchmarks run **sender and receiver on separate OS threads** with separate
runtimes, so results reflect real kernel TCP/IPC round-trips. The timer lives on
the receiver side for throughput tests. Both sides are given identical
methodology - same number of operations per message, same warmup structure.

The three runtime backends run the identical suite (compio uses io_uring, tokio
and smol use epoll). On these single-flow loopback microbenchmarks no backend
leads everywhere: compio is ahead on coalesced throughput and tokio has the
shortest round trip. A one-connection run does not exercise io_uring's other
strengths (batched submission, registered buffers, many concurrent
connections). The criterion tables below (throughput, latency, IPC-vs-TCP,
pipelined, fan-out and fan-in) are 1.0 numbers, measured together in one run on
a quiet machine with the receiver's timer starting on a live connection. The
two-thread benches ran pinned to two performance cores (`taskset -c 0,2`) and
the worker-pool benches unpinned. The cross-process multi-implementation
comparison tables further down come from a separate harness and are kept for
shape, not as absolute reference-machine numbers;
[docs/performance.md](../docs/performance.md) is the canonical breakdown.

Hardware: Intel Core i7-1355U (12 threads), Linux 7.0, release build, `rustc 1.98`.

---

## Measured Results

Each heading names the bench target; the full commands are under
[Running the Benchmarks](#running-the-benchmarks).

### Throughput - `--bench throughput`

PUSH/PULL one-way pipeline, 10 000 messages per iteration.

**eager** - default, one kernel write per `send()`:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 565 K | 568 K | **595 K** |
| 256 B | **556 K** | 531 K | 553 K |
| 1 KB | **494 K** | 478 K | **494 K** |
| 4 KB | 408 K | 406 K | **440 K** |
| 16 KB | 280 K | **335 K** | 327 K |

**coalesced** - `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | **30.8 M** | 27.0 M | 27.1 M |
| 256 B | **15.3 M** | 14.3 M | 14.4 M |
| 1 KB | **7.04 M** | 6.09 M | 6.10 M |
| 4 KB | **2.06 M** | 1.89 M | 1.88 M |
| 16 KB | **550 K** | 409 K | 381 K |

**rust-zmq (libzmq)**:

| Message size | msg/s |
|---|---|
| 64 B | 6.79 M |
| 256 B | 4.47 M |
| 1 KB | 2.05 M |
| 4 KB | 602 K |
| 16 KB | 150 K |

Eager mode is a latency tool (each `send()` goes out immediately, one syscall per
message), not a throughput one. On a bulk one-way firehose, libzmq's internal
IO-thread batching wins small messages (6.79 M vs 565-595 K at 64 B, ~12x) and
still leads at 4 KB (602 K vs 406-440 K); monocoque leads ~1.9-2.2x at 16 KB,
where vectored writes avoid the copy. With write coalescing, all three backends
beat libzmq by ~2.5-4.5x across the range (at 64 B ~4.5x compio, ~4.0x tokio and
smol); compio leads on these single-flow runs. Reach for eager when per-message
delivery latency matters; turn on coalescing for small-message throughput.

The rust-zmq column is measured with the receiver timer starting on a live
connection (one warmup message before the clock), the same as the monocoque path;
an earlier version started the zmq timer before the sender connected, which
understated libzmq at small sizes.

`recv()` now returns a borrowed `MsgRef` and does not allocate; `recv_owned()`
returns an owned `Msg`. The `push_pull_coalesced_owned` case measures the cost
of taking ownership against `push_pull_coalesced`.

Measured on 0.4 with the removed `recv_into` API: there the PULL side allocated
a `Vec<Bytes>` per message, and the `push_pull_coalesced_recv_into` case
(receiving into a reused buffer) showed ~1.10x at 64 B (14.1 M to 15.6 M on
compio, 18.0 M to 19.1 M on tokio) and ~9% at 256 B over `recv()` (the gain
tapered as messages grew and the path became bandwidth-bound). That case no
longer exists. See `docs/performance.md` for details.

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

### Latency - `--bench latency`

REQ/REP steady-state round-trip on TCP loopback. The connection is established
once (plus 200 warmup rounds) outside the timer, then N back-to-back round-trips
are timed; socket teardown and thread join happen after the clock stops.

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 7.2 µs | **5.2 µs** | 6.8 µs | 19.8 µs |
| 256 B | 7.4 µs | **5.4 µs** | 7.1 µs | 20.1 µs |
| 1 KB | 7.9 µs | **6.3 µs** | 8.1 µs | 21.0 µs |

The same round trip over a Unix socket (`req_rep_ipc`):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 4.8 µs | **3.1 µs** | 4.7 µs |
| 256 B | 4.8 µs | **3.2 µs** | 4.6 µs |
| 1 KB | 4.9 µs | **3.8 µs** | 5.9 µs |

All three backends are ~2.5-3.8x lower round-trip latency than libzmq's ~20 µs
(tokio ~5.3 µs, smol ~7.0 µs, compio ~7.3 µs at the two smaller sizes). The
advantage comes from doing the I/O inline on one thread, with no handoff to a
background IO thread the way libzmq does, and from looking for the reply
before sleeping. tokio leads here: on a single-flow round trip an epoll wakeup
plus a plain read is leaner than submitting and reaping a completion.

---

### IPC vs TCP - `--bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 55 µs | 56 µs | 70 µs |
| compio IPC | 61 µs | 64 µs | 65 µs |
| tokio TCP | 23 µs | 24 µs | 33 µs |
| tokio IPC | 29 µs | 28 µs | 28 µs |
| smol TCP | 37 µs | 33 µs | 37 µs |
| smol IPC | 33 µs | 33 µs | 33 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 530 K msg/s | 534 K msg/s | 444 K msg/s |
| compio IPC | 2.31 M msg/s | 2.07 M msg/s | 1.88 M msg/s |
| tokio TCP | 567 K msg/s | 541 K msg/s | 455 K msg/s |
| tokio IPC | 2.23 M msg/s | 2.04 M msg/s | 2.04 M msg/s |
| smol TCP | 564 K msg/s | 560 K msg/s | 486 K msg/s |
| smol IPC | 2.17 M msg/s | 1.99 M msg/s | 1.86 M msg/s |

IPC is ~4x faster than TCP loopback for throughput on every backend (~4.4x
compio, ~3.9x tokio and smol at 64 B), because Unix sockets have lower
per-syscall overhead and no TCP framing. The latency table above includes
per-iteration setup and teardown, which dominate it: the round trip itself is a
few microseconds of each figure (see the steady-state `latency` tables), so
read it as the cost of a short-lived connection.

---

### Pipelined coalesced batches - `--bench pipelined_throughput`

DEALER/ROUTER with write coalescing, a batch of 100 `send()` calls then one
`flush()`, 10 000 total messages. This is a monocoque-only benchmark.

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 3.20 M (195 MiB/s) | **3.43 M** (209 MiB/s) | 3.33 M (203 MiB/s) |
| 256 B | 2.87 M (700 MiB/s) | **2.97 M** (726 MiB/s) | 2.88 M (704 MiB/s) |
| 1 KB | 2.12 M (2.02 GiB/s) | **2.26 M** (2.15 GiB/s) | 2.17 M (2.07 GiB/s) |
| 4 KB | 878 K (3.35 GiB/s) | **945 K** (3.60 GiB/s) | 930 K (3.55 GiB/s) |
| 16 KB | 267 K (4.07 GiB/s) | **284 K** (4.33 GiB/s) | 265 K (4.04 GiB/s) |

---

### Fan-out / fan-in worker pools - `--bench fanout_fanin`

Monocoque-only throughput for the two pool topologies, `WORKERS = 4`, 10 000
messages per iteration split evenly across the pool. `fanout` is one PUSH socket
with four peers, round-robining to four PULL workers (timed across the workers,
so the cost is when the last message lands); `fanin` is four PUSH workers merged
by one PULL socket with four peers (timed on the sink). This is the in-process
counterpart to the bench peer's `push-fanout` / `pull-fanin` subcommands. The
msg/s figure is aggregate delivered throughput across the pool; bandwidth is the
matching payload rate.

`PushSocket` and `PullSocket` load balance and fair-queue across peers
natively, and the bench uses them (0.4 had separate `PushFanOut` and
`PullFanIn` types).

Fan-out (one ventilator, four PULL workers), coalescing senders (msg/s;
bandwidth is msg/s x frame size):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 12.8 M | **13.6 M** | 10.5 M |
| 1 KB | 3.06 M | **3.29 M** | 3.10 M |
| 16 KB | 393 K | **430 K** | 345 K |

The ventilator round-robins one message at a time; with coalescing each worker's
buffer flushes at the 64 KB threshold, so the writes stay batched while the four
workers receive interleaved and in parallel. Handing each worker its whole share
in one batched write instead serializes the pool (worker 1 waits for worker 0's
entire share) and is markedly slower at large messages, so the per-message path
is the one kept.

Fan-in, coalescing senders (large kernel-read batches):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 16.2 M | **18.3 M** | 18.1 M |
| 1 KB | **4.33 M** | 3.96 M | 4.23 M |
| 16 KB | **549 K** | 343 K | 447 K |

At larger sizes the path is bandwidth-bound, so the sender mode matters less.

Fan-in, eager senders (one write per message):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 1.37 M | **1.60 M** | 1.34 M |
| 1 KB | 1.18 M | **2.53 M** | 1.78 M |
| 16 KB | **451 K** | 306 K | 443 K |

With eager senders the four PUSH workers cap throughput at their per-message write
rate, well below what the sink can drain, so the sink is no longer the bottleneck.
The 1 KB eager row is noisy run to run (the workers and sink trade the
bottleneck), so treat it as approximate.

---

### Vectored writes, PUB coalescing

These paths were measured on 0.4 with a one-off harness (not part of the
criterion suite) that toggled each change via its public knob so the effect was
isolated. Numbers below are a
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

On 0.4 the crossover was ~32 KB, which set that release's 32 KiB
`vectored_write_threshold`; below it the contiguous copy plus one `write` beat a
two-segment `writev`. The default is now 16 KiB.

**PUB→SUB delivered broadcast, 1 subscriber** (coalescing on): 64 B ~174 K
msg/s, 1 KB ~161 K msg/s.

---

## Benchmark Methodology

### Why these designs are fair

**Separate OS threads**: both sides run on different threads with different
runtimes of the selected backend. There is genuine TCP between them - messages pass through
the kernel network stack and loopback device.

**Same work per message**: zmq PUSH/PULL does one `send` / one `recv_bytes`
per message. monocoque does one `send` / one `recv` per message. No artificial
asymmetry.

**No setup in the timed window**: the zmq benchmark uses a 5 ms sleep before
connecting the PUSH socket so the PULL socket registers with the kernel first, but
the PULL receives one warmup message before starting its timer, so that sleep and
the connection setup fall outside the measured window. This matches the monocoque
path, which starts its timer only after the ZMTP handshake and one warmup message. (An
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

- Published numbers for PUB fan-out to **many** subscribers (single-subscriber
  delivered throughput is measured above; the `fanout_slow_subscriber` bench
  drives 100 SUB peers but its results are not collected here yet)
- Fan-out / fan-in worker pools **against other implementations**: the
  in-process `fanout_fanin` criterion bench covers monocoque, and the
  `push-fanout` / `pull-fanin` bench-peer subcommands exist, but a measured
  cross-implementation comparison has not been collected yet
- IPC coalesced throughput against competing IPC implementations

The `patterns` and `fanout_slow_subscriber` benches run each PUB case twice,
with an eager publisher and with a coalescing one (`_coalesced`). 0.4's PUB
batched on worker threads without being asked, so compare 0.4 against the
coalesced cases.

---

## Running the Benchmarks

All commands below work from either the workspace root or the `monocoque/`
subdirectory. The package is `monocoque-rs`. Every bench except `allocation`
needs the `zmq` feature (`allocation` has no `required-features`).

```bash
# Run the comparison suites (throughput, latency, IPC, pipelined, patterns)
# Takes ~20 minutes.
cargo bench -p monocoque-rs --features zmq \
    --bench throughput --bench latency --bench ipc_vs_tcp \
    --bench pipelined_throughput --bench patterns

# Run the allocator micro-benchmarks (no zmq dependency)
cargo bench -p monocoque-rs --bench allocation

# Individual comparison suite
cargo bench -p monocoque-rs --bench throughput --features zmq
cargo bench -p monocoque-rs --bench latency --features zmq
cargo bench -p monocoque-rs --bench ipc_vs_tcp --features zmq
cargo bench -p monocoque-rs --bench pipelined_throughput --features zmq
cargo bench -p monocoque-rs --bench patterns --features zmq
cargo bench -p monocoque-rs --bench fanout_fanin --features zmq

# Other benches
cargo bench -p monocoque-rs --bench fanout_slow_subscriber --features zmq
cargo bench -p monocoque-rs --bench router_n_peers --features zmq
cargo bench -p monocoque-rs --bench inproc_socket --features zmq
cargo bench -p monocoque-rs --bench multithreaded --features zmq

# Filter to a specific case
cargo bench -p monocoque-rs --bench throughput --features zmq -- "throughput/monocoque-compio/push_pull_coalesced"

# Quick smoke-test (no timing, just checks nothing panics)
cargo bench -p monocoque-rs --bench throughput --features zmq -- --test

# Cross-implementation comparison bench peer
cd scripts/monocoque_bench_peer && cargo build --release
```

`BENCH_SECS=2` shortens the `throughput` bench's measurement from 15 seconds
per size while iterating; publish numbers from the default.

On a CPU with performance and efficiency cores, pin a run to two performance
cores (`taskset -c 0,2 ...`, after checking `lscpu -e`): left to the
scheduler, the same build moved by 10% or more from run to run as threads
landed on different core types.

For stable numbers, avoid running other benchmarks in parallel and disable
CPU frequency scaling if available:

```bash
sudo cpupower frequency-set --governor performance
cargo bench -p monocoque-rs --features zmq \
    --bench throughput --bench latency --bench ipc_vs_tcp \
    --bench pipelined_throughput --bench patterns
```
