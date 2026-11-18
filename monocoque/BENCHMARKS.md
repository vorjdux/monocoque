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
| 64 B | 555 K | 566 K | **583 K** |
| 256 B | 548 K | 548 K | **555 K** |
| 1 KB | 451 K | 483 K | **495 K** |
| 4 KB | 397 K | 425 K | **444 K** |
| 16 KB | 277 K | 303 K | **330 K** |

**coalesced** - `with_write_coalescing(true)`, 64 KB flush threshold:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | **31.0 M** | 27.7 M | 27.5 M |
| 256 B | **15.4 M** | 14.9 M | 14.9 M |
| 1 KB | **7.22 M** | 6.35 M | 6.35 M |
| 4 KB | **2.19 M** | 2.00 M | 1.99 M |
| 16 KB | **582 K** | 474 K | 471 K |

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
IO-thread batching wins small messages (6.79 M vs 555-583 K at 64 B, ~12x) and
still leads at 4 KB (602 K vs 397-444 K); monocoque leads ~1.8-2.2x at 16 KB,
where vectored writes avoid the copy. With write coalescing, all three backends
beat libzmq by ~3-4.6x across the range (at 64 B ~4.6x compio, ~4.1x tokio and
smol); compio leads on these single-flow runs. Reach for eager when per-message
delivery latency matters; turn on coalescing for small-message throughput.

The rust-zmq column is measured with the receiver timer starting on a live
connection (one warmup message before the clock), the same as the monocoque path;
an earlier version started the zmq timer before the sender connected, which
understated libzmq at small sizes.

`recv()` returns a borrowed `MsgRef` and does not allocate; `recv_owned()`
returns an owned `Msg`. The `push_pull_coalesced_owned` case measures the cost
of taking ownership against `push_pull_coalesced`. See
[Borrowed receive](../docs/performance.md#borrowed-receive) for when each
fits.

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
| 64 B | 7.3 µs | **5.3 µs** | 6.8 µs | 19.8 µs |
| 256 B | 7.4 µs | **5.5 µs** | 7.2 µs | 20.1 µs |
| 1 KB | 7.6 µs | **6.0 µs** | 8.1 µs | 21.0 µs |

The same round trip over a Unix socket (`req_rep_ipc`):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 4.6 µs | **3.0 µs** | 4.6 µs |
| 256 B | 4.7 µs | **3.1 µs** | 4.5 µs |
| 1 KB | 4.9 µs | **3.7 µs** | 6.1 µs |

All three backends are ~2.6-3.8x lower round-trip latency than libzmq's ~20 µs
(tokio ~5.4 µs, smol ~7.0 µs, compio ~7.4 µs at the two smaller sizes). The
advantage comes from doing the I/O inline on one thread, with no handoff to a
background IO thread the way libzmq does, and from looking for the reply
before sleeping. tokio leads here: on a single-flow round trip an epoll wakeup
plus a plain read is leaner than submitting and reaping a completion.

---

### IPC vs TCP - `--bench ipc_vs_tcp`

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 57 µs | 57 µs | 57 µs |
| compio IPC | 54 µs | 56 µs | 54 µs |
| tokio TCP | 22 µs | 23 µs | 30 µs |
| tokio IPC | 24 µs | 24 µs | 24 µs |
| smol TCP | 28 µs | 28 µs | 35 µs |
| smol IPC | 27 µs | 29 µs | 29 µs |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 542 K msg/s | 517 K msg/s | 439 K msg/s |
| compio IPC | 2.40 M msg/s | 2.16 M msg/s | 1.97 M msg/s |
| tokio TCP | 581 K msg/s | 541 K msg/s | 476 K msg/s |
| tokio IPC | 2.24 M msg/s | 2.05 M msg/s | 2.03 M msg/s |
| smol TCP | 587 K msg/s | 550 K msg/s | 491 K msg/s |
| smol IPC | 2.16 M msg/s | 1.99 M msg/s | 1.84 M msg/s |

IPC is ~4x faster than TCP loopback for throughput on every backend (~4.4x
compio, ~3.9x tokio and ~3.7x smol at 64 B), because Unix sockets have lower
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
| 64 B | 4.02 M (245 MiB/s) | **4.34 M** (265 MiB/s) | 4.29 M (262 MiB/s) |
| 256 B | 2.95 M (719 MiB/s) | **3.03 M** (740 MiB/s) | **3.03 M** (740 MiB/s) |
| 1 KB | 2.18 M (2.08 GiB/s) | **2.29 M** (2.18 GiB/s) | 2.21 M (2.10 GiB/s) |
| 4 KB | 912 K (3.48 GiB/s) | **941 K** (3.59 GiB/s) | 917 K (3.50 GiB/s) |
| 16 KB | 280 K (4.28 GiB/s) | 252 K (3.84 GiB/s) | **283 K** (4.32 GiB/s) |

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
| 64 B | **21.9 M** | 21.5 M | 17.2 M |
| 1 KB | 5.40 M | **5.52 M** | 5.05 M |
| 16 KB | 498 K | **526 K** | 497 K |

The ventilator round-robins one message at a time; with coalescing each worker's
buffer flushes at the 64 KB threshold, so the writes stay batched while the four
workers receive interleaved and in parallel. Handing each worker its whole share
in one batched write instead serializes the pool (worker 1 waits for worker 0's
entire share) and is markedly slower at large messages, so the per-message path
is the one kept.

Fan-in, coalescing senders (large kernel-read batches):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 18.2 M | 18.5 M | **20.8 M** |
| 1 KB | **7.75 M** | 6.06 M | 7.60 M |
| 16 KB | **581 K** | 277 K | 320 K |

At larger sizes the path is bandwidth-bound, so the sender mode matters less.

Fan-in, eager senders (one write per message):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | **1.11 M** | 1.07 M | 1.09 M |
| 1 KB | 996 K | 1.02 M | **1.03 M** |
| 16 KB | **535 K** | 275 K | 316 K |

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

**PUB→SUB delivered broadcast, 1 subscriber** (coalescing on), measured on 0.4
with the same one-off harness: 64 B ~174 K msg/s, 1 KB ~161 K msg/s. This is
not a 1.0 figure and does not compare with the 1.0 PUB/SUB tables in
[docs/performance.md](../docs/performance.md), which come from the criterion
suite.

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

**The bench peer is written against the 0.4 API and does not build against
1.0.** It is its own Cargo workspace, not part of the 1.0 workspace, and its
source still uses calls that 1.0 removed (the `connect` and `from_tcp`
constructors, `recv_into` and `try_recv_into`, `PushFanOut` and `PullFanIn`,
`Vec<Bytes>` messages). Everything in this section describes it as it runs on
0.4: build it from a 0.4 checkout (`git checkout v0.4.2`), not from this tree.
For 1.0, cross-implementation runs go through
[zmq-arena](https://vorjdux.github.io/zmq-arena/).

`scripts/monocoque_bench_peer/` is a standalone Rust binary that implements the same two-process
wire protocol as the other bench peers in the cross-implementation comparison suite
(libzmq, zmqrs\_bench\_peer, rzmq\_bench\_peer). It can participate directly in
the external comparison harness that drives those peers side by side.

Key design choices in the bench peer:

- `push` uses write coalescing (flushed every 64 messages) to show monocoque's
  maximum throughput. `push-eager` uses the default mode for latency-tuned
  scenarios.
- `pull` receives into one reused buffer and, after each receive that waited,
  takes whatever else is already queued without waiting, so several messages
  that the kernel delivered in one read cost one io_uring submission.
- No warmup sleep on the pull/req side. (A sleep fills the kernel send buffer
  and deadlocks monocoque's single-threaded runtime on a blocked write.)
- IPC subcommands (`push-ipc`, `pull-ipc`, `rep-ipc`, `req-ipc`) use Unix
  domain sockets; the bound path is printed as `PATH <p>` on stdout.
- Fan-out / fan-in subcommands drive the worker-pool topologies: `push-fanout`
  binds a ventilator that round-robins to N PULL workers, `pull-fanin` binds a
  sink that merges N PUSH workers, and `push-connect` is the connecting PUSH used
  as a fan-in worker. Fan-out workers reuse the plain `pull` subcommand.

```bash
# In a 0.4 checkout: build the bench peer
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

- Published numbers for PUB fan-out to **many** subscribers (the only
  single-subscriber delivered figure above is the 0.4 one from the one-off
  harness; 1.0 fan-out to one and four subscribers is in
  [docs/performance.md](../docs/performance.md); the `fanout_slow_subscriber`
  bench drives 100 SUB peers but its results are not collected here yet)
- Fan-out / fan-in worker pools **against other implementations**: the
  in-process `fanout_fanin` criterion bench covers monocoque, and the
  `push-fanout` / `pull-fanin` bench-peer subcommands exist on 0.4, but a measured
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
```

The cross-implementation bench peer in `scripts/monocoque_bench_peer` is not
in this list: it targets the 0.4 API and does not build against 1.0 (see
[Cross-implementation bench peer](#cross-implementation-bench-peer)).

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

---

## Benchmarking on Scaleway

`scripts/scaleway_bench.sh` rents one Scaleway Elastic Metal server, runs the
benchmarks on it, copies the results back and deletes the server. It uses the
same zone (`fr-par-1`), the same offer (`EM-B130E-NVME`: AMD EPYC 4245P, 6
cores, 32 GB, NVMe) and the same OS (Ubuntu 26.04 LTS) every time, so runs
weeks apart are on comparable hardware. The server gets SMT turned off and the
`performance` governor before anything is measured.

Six cores are enough for everything except the 8-pair case of the
`multithreaded` bench, which leaves itself out on a machine with fewer CPUs
than pairs. `--offer` picks another machine, but numbers from two different
offers are not comparable with each other.

> **Only deleting the server stops the billing.** A server that is powered off
> or stopped is still charged by the hour. The script deletes it on every way
> out, including Ctrl-C, and confirms that it is gone. If a run is ever killed
> hard (`kill -9`, a lost machine), delete the server by hand as shown below.

### Prerequisites

- The [Scaleway CLI](https://github.com/scaleway/scaleway-cli) (`scw`), plus
  `jq`, `curl` and `ssh`.
- An API key in the environment: `SCW_ACCESS_KEY`, `SCW_SECRET_KEY`,
  `SCW_DEFAULT_PROJECT_ID` and `SCW_DEFAULT_ORGANIZATION_ID`. The script reads
  them from there only and writes them nowhere.
- An SSH key whose public half is registered in the Scaleway project. The
  private key is `~/.ssh/id_ed25519` unless `SSH_KEY` names another. It must
  have no passphrase.
- **A billing alert of about €15 a month** in the Scaleway console (Billing,
  then Alerts). It is the one safeguard that does not depend on this
  repository's code working.

### Cost

The offer is €0.247 an hour. A run costs under €1: installing the OS takes
most of the first hour, and the suite takes a little over one more. The script
prints the billed time and an estimate when it finishes.

### Running it

```bash
# See every command without creating anything (needs no credentials)
scripts/scaleway_bench.sh --dry-run

# The full suite on HEAD: all three backends, pinned
scripts/scaleway_bench.sh

# The working revision against another, in alternating rounds
scripts/scaleway_bench.sh --mode pair --against v0.4.2 --rounds 5

# Instruction counts against the committed baseline
scripts/scaleway_bench.sh --mode instr

# Any other command, run in the unpacked source tree
scripts/scaleway_bench.sh --mode arena --arena-cmd 'make dev-bench'
```

`--ref` picks the revision to test (the committed tree is shipped, never
uncommitted changes), `--backend` and `--bench` narrow the run, and `--help`
lists the rest. The source goes to the server as a `git archive` tarball, so
the server never holds a GitHub credential.

Results land in `bench-results/<UTC date>-<UTC time>-<short sha>-<mode>/`, which git
ignores: the criterion data, every log, the host description (`lscpu`, kernel,
microcode, SMT, governor, boost), the load averages before and after, and a
`run.json` with the zone, offer, server ID, revisions, times and exit codes.

### How long a server can live

A server never outlives its maximum lifetime, which is the script's hard cap
plus 40 minutes (4 h 40 min by default; `--max-lifetime MIN` changes it). Four
things enforce that, each independent of the one before:

1. The script deletes the server on every exit and confirms that it is gone.
2. A watchdog, started detached just before the server is created, deletes it
   as soon as the run is gone without having deleted it (`kill -9`, a closed
   terminal), and at the maximum lifetime even if the run is still alive and
   stuck. It applies to `--keep` too.
3. In CI, a final step deletes the run's server even when the job is
   cancelled, and an hourly job deletes any tagged server past its deadline.
   Each server carries that deadline, its own maximum lifetime, as a second
   tag, so a run given more than the usual time is not deleted while it
   works. A tagged server with no deadline is deleted at 4 hours old, and one
   with a deadline at 24 hours old whatever the deadline says.
4. The billing alert.

Pair mode exits non-zero, after an `INCOMPLETE` list in its log, when a
benchmark did not build, a run failed or a case has fewer rounds than were
asked for. The results are still copied back.

The watchdog runs on the machine that started the run, so it stops if that
machine is shut down or loses its network. The hourly CI job covers that case
once the workflow is on the default branch with its secrets set; until then,
after such an interruption, run `--reap --max-age 0` by hand.

Nothing on the server can end the billing: it would need a Scaleway
credential there, and powering the server off does not stop the charge.

### Leftover servers

Every server the script creates is tagged `monocoque-bench`. Its zone and name
are written to `.scaleway-bench-server` before the create call, and its ID as
soon as that call answers, so a run cut off in between still leaves a pointer.
To delete whatever is left:

```bash
scripts/scaleway_bench.sh --reap --max-age 0     # every tagged server, now
scripts/scaleway_bench.sh --reap                 # only those past their deadline
scripts/scaleway_bench.sh --reap --max-age 0 --server-name NAME   # one, by name
scw baremetal server list zone=all tags.0=monocoque-bench   # check by hand
```

`--reap` exits non-zero if a server is still listed afterwards. `--keep`
leaves the server running on purpose, for looking at a result on the machine
itself, and prints the command that deletes it.

### Weekly run

`.github/workflows/scaleway-bench.yml` runs the suite once a week and can be
started by hand with a mode, a revision and a backend. It uploads the results
as a workflow artifact kept for 90 days, deletes its server in a final step
that runs even when the job is cancelled, and runs `--reap` every hour as a
separate job. It needs the four `SCW_*` values and `SSH_PRIVATE_KEY` as
repository secrets.
