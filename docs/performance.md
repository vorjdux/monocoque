# Performance

## Where the numbers live

Two kinds of measurement, kept apart on purpose.

**Comparisons against other implementations live in
[zmq-arena](https://vorjdux.github.io/zmq-arena/)**, a standalone harness that
runs fifteen ZeroMQ implementations as separate processes on bare metal, each in
a cgroup v2 slice with a per-run network namespace, six replicates per cell with
outlier dropping. It records syscall counts, CPU seconds split by sender and
receiver, peak RSS and run-to-run spread alongside throughput and latency, and
publishes the raw JSON for every run. That is the place to look for how monocoque
compares to libzmq, rzmq, zmq.rs, celerity or the libzmq bindings, on TCP or
IPC, across payload sizes and peer counts, and for the cells where monocoque does
not come out ahead.

Do not expect the arena's absolute figures to match a local `cargo bench` run.
The arena measures inside a network namespace with cgroup isolation, which costs
throughput and adds latency; that is the price of comparing implementations
fairly rather than flattering any one of them.

**Regression baselines stay in this repository**, because they gate every commit
rather than describe a competitive position:

- `benches/instr_hotpath.rs` counts instructions on the IO-free hot path under
  callgrind and fails a run more than 5% over baseline
- `tests/hotpath_alloc.rs` asserts the per-message allocation count
- `tests/socket_footprint_bound.rs` bounds idle per-socket resident memory
- `docs/perf/baselines/` holds the per-commit snapshots those gates compare to

The rest of this document is tuning guidance: which knobs exist, what each one
trades, and when to reach for it.

## Local benchmark suite

Hardware for the local figures quoted below: Intel Core i7-1355U (12 threads),
Linux 6.17, release build, `rustc 1.96`. Each benchmark runs sender and receiver
on **separate OS threads** with separate runtimes, so the numbers reflect real
kernel TCP/IPC round-trips, not cooperative task switching within a single
runtime. These are single-connection loopback microbenchmarks used to compare
monocoque against itself across releases and knob settings, not against other
implementations.

Monocoque has three runtime backends: compio (io_uring), tokio (epoll), and smol
(async-io/epoll). Each runs the identical suite, and all three are covered in the
tables below. Pick the backend with a Cargo feature:

```bash
cargo bench --features zmq                                       # compio (default)
cargo bench --no-default-features --features runtime-tokio,zmq   # tokio
cargo bench --no-default-features --features runtime-smol,zmq    # smol
```

**What these numbers do and do not show.** These are single-connection loopback
microbenchmarks. On this workload the tokio/epoll backend still edges
compio/io_uring, though the compio 0.19 upgrade narrowed the gap sharply (compio
now leads smol and is close to tokio): a single-flow ping-pong does not exercise
io_uring's strengths (batched submission, registered buffers, scaling across many
concurrent connections), so it just pays io_uring's per-operation submission
overhead while epoll plus a plain `write` stays leaner. io_uring's advantage shows up on real
network I/O and high connection counts, which these benches do not cover.
Benchmark on your own workload before picking a backend.

All columns were re-measured together for the 0.4.0 release on this box on a
quiet machine, on the same corrected live-connection timer (see below). Latency
is a fresh steady-state run for all columns.

Throughput timer lives on the receiver side and starts on a **live** connection:
the receiver takes one warmup message before the clock starts, so connection setup
is excluded. Latency is measured steady-state: the connection is
established once (plus 200 warmup rounds) outside the timer, then N back-to-back
round-trips are timed, with socket teardown and thread join happening after the
timer stops.

---

### Throughput - PUSH/PULL one-way, 10 000 messages

`eager` - default mode, one kernel write per `send()`.
`coalesced` - `with_write_coalescing(true)`, messages accumulate in a 64 KB
buffer flushed in one syscall; call `flush()` after the last send.

Eager mode (one syscall per message):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 481 K msg/s | 520 K msg/s | 437 K msg/s |
| 256 B | 481 K msg/s | 513 K msg/s | 436 K msg/s |
| 1 KB | 440 K msg/s | 479 K msg/s | 396 K msg/s |
| 4 KB | 404 K msg/s | 419 K msg/s | 377 K msg/s |
| 16 KB | 335 K msg/s | 300 K msg/s | 279 K msg/s |

Write coalescing (batched into 64 KB writes):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 14.1 M msg/s | **18.0 M msg/s** | 12.9 M msg/s |
| 256 B | 8.3 M msg/s | **12.6 M msg/s** | 8.5 M msg/s |
| 1 KB | 3.5 M msg/s | **5.7 M msg/s** | 3.3 M msg/s |
| 4 KB | 1.15 M msg/s | **1.70 M msg/s** | 1.15 M msg/s |
| 16 KB | 356 K msg/s | **454 K msg/s** | 368 K msg/s |

Eager mode is a **latency** tool, not a throughput one. Each `send()` puts that
message on the wire immediately with its own syscall, so you control exactly when
every message is delivered instead of waiting for a buffer to fill. That is what
you want for request/reply, RPC, and interactive protocols, where a message must
go out *now* and you shape the response per send. The throughput bench below runs
a firehose of one-way messages, which is the opposite of eager mode's purpose, so
read these numbers as "what happens if you eager-send a bulk stream," not as a
verdict on the mode.

The eager-versus-coalesced gap is the point of these two tables. At 64 B,
coalescing moves roughly 30x more messages, because eager pays one kernel write
per `send()` while coalescing amortizes hundreds of messages per syscall. The gap
narrows as messages grow: by 16 KB a single payload nearly fills a write on its
own, and the two modes converge. The takeaway: if you are streaming small
messages in bulk, turn on write coalescing; reach for eager when per-message
delivery latency and control matter more than aggregate rate.

For how either mode compares against other implementations, see
[zmq-arena](https://vorjdux.github.io/zmq-arena/).

Write coalescing batches ~970 x 64 B messages (or ~240 x 256 B) into one
`write_all()` call, eliminating the per-message kernel boundary crossing. tokio's
epoll path still leads on these single-flow loopback runs, but the compio 0.19
upgrade lifted compio above smol (io_uring's per-op submission overhead against
epoll's readiness model, now much narrower). Coalescing here is explicit rather
than a scheduling side effect, and achieves a high batch ratio with zero
intermediate copies.

For **large** frames, eager mode no longer copies the body into the send buffer:
at or above `vectored_write_threshold` (default 16 KiB) it writes the header and the
`Bytes` body as an iovec (`writev`), so the payload is never copied on its way to
the kernel. See [Vectored writes](#vectored-writes-large-frames) below.

---

### Throughput - DEALER/ROUTER batch API, 10 000 messages, batches of 100

The pipelined benchmark sends each batch with write coalescing on, then calls
`flush()`, so N messages are encoded and go out in one write with the caller
choosing the batch boundaries:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 3.34 M msg/s (204 MiB/s) | 3.12 M msg/s (190 MiB/s) | 2.31 M msg/s (141 MiB/s) |
| 256 B | 2.72 M msg/s (665 MiB/s) | 2.62 M msg/s (639 MiB/s) | 1.94 M msg/s (475 MiB/s) |
| 1 KB | 1.71 M msg/s (1.63 GiB/s) | 1.66 M msg/s (1.58 GiB/s) | 1.40 M msg/s (1.34 GiB/s) |
| 4 KB | 713 K msg/s (2.72 GiB/s) | 707 K msg/s (2.70 GiB/s) | 647 K msg/s (2.47 GiB/s) |
| 16 KB | 146 K msg/s (2.23 GiB/s) | 157 K msg/s (2.39 GiB/s) | 159 K msg/s (2.42 GiB/s) |

These figures were measured on 0.4, when the bench used the since-removed
`send_buffered` batch call. They have not been re-measured with coalesced
`send` + `flush`; treat them as the 0.4 result, not this release's.

---

### Latency - REQ/REP steady-state round-trip

The connection is established once (plus 200 warmup rounds) outside the timer;
then N back-to-back REQ/REP round-trips are timed on that persistent connection,
with socket teardown and thread join happening after the clock stops. These are
true steady-state round-trip times, not connection setup or teardown.

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 8.4 µs | 9.8 µs | 12.4 µs |
| 256 B | 8.5 µs | 9.5 µs | 12.6 µs |
| 1 KB | 8.7 µs | 9.7 µs | 13.5 µs |

compio and tokio are lowest (compio ~8.5 µs after the 0.19 upgrade), and smol
~12.8 µs (async-io's readiness wakeup costs it a couple of microseconds over the
other two). The short round-trip comes from doing the I/O inline on the same
thread, with no handoff to a separate IO thread on the send and recv path.
For how this compares against other implementations, see
[zmq-arena](https://vorjdux.github.io/zmq-arena/). compio and tokio are
neck and neck on this single-flow round-trip; the compio 0.19 upgrade brought
io_uring's submit/reap cost down to par with (and often just under) an epoll
wakeup, and on real network I/O and high connection counts io_uring pulls ahead.

---

### IPC vs TCP loopback

Unix domain sockets (`UnixStream`) skip the IP stack entirely; on Linux they
share kernel buffers without copying.

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 51 µs | 53 µs | 56 µs |
| compio IPC | 96 µs | 108 µs | 103 µs |
| tokio TCP | 47 µs | 52 µs | 56 µs |
| tokio IPC | 59 µs | 69 µs | 61 µs |
| smol TCP | 81 µs | 82 µs | 92 µs |
| smol IPC | 83 µs | 83 µs | 90 µs |

Unlike the steady-state latency table above, this bench still includes
per-iteration teardown (FIN/close plus thread join), which dominates the
measurement. On tokio and smol, IPC and TCP land within each other's band. On
compio, IPC round-trip latency runs higher than TCP: io_uring's per-operation
submit/reap overhead is a larger fraction of the very short Unix-socket round
trip, where the kernel path is fastest, so the fixed op cost dominates. That same
op cost is amortized away on the throughput axis below, where compio IPC is
fastest. IPC's per-message advantage shows up on throughput, not on a single
teardown-heavy round trip.

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 488 K msg/s | 489 K msg/s | 458 K msg/s |
| compio IPC | 1.51 M msg/s | 1.42 M msg/s | 1.30 M msg/s |
| tokio TCP | 513 K msg/s | 523 K msg/s | 473 K msg/s |
| tokio IPC | 1.53 M msg/s | 1.46 M msg/s | 1.37 M msg/s |
| smol TCP | 438 K msg/s | 437 K msg/s | 401 K msg/s |
| smol IPC | 1.44 M msg/s | 1.32 M msg/s | 1.18 M msg/s |

IPC throughput is ~3x TCP loopback on every backend (compio moved up to ~3.1x
with the 0.19 upgrade, from ~2.1x), because Unix sockets have lower per-syscall
overhead and no TCP framing cost.

---

### PUB/SUB patterns

Sender and subscribers run on separate OS threads.

**Fan-out** (single subscriber, 256 B messages):

| | Per 100-msg batch | Throughput |
|---|---|---|
| monocoque (compio) | 36 µs | 2.76 M msg/s |
| monocoque (tokio) | 37 µs | 2.70 M msg/s |
| monocoque (smol) | 36 µs | 2.80 M msg/s |

**Topic filtering** (10% of messages match the subscription):

| | Per 100-msg batch | Throughput |
|---|---|---|
| monocoque (compio) | 4.6 µs | 21.9 M msg/s |
| monocoque (tokio) | 4.7 µs | 21.5 M msg/s |
| monocoque (smol) | 4.7 µs | 21.2 M msg/s |

The three backends are within noise of each other on both patterns. Topic
filtering is a tight microbenchmark where a few hundred nanoseconds of filter
cost dominates, so its numbers move run to run; read them as an order of
magnitude rather than a ranking. Cross-implementation PUB/SUB results, including
runs at 4 and 32 subscribers, are in
[zmq-arena](https://vorjdux.github.io/zmq-arena/).

---

## Write coalescing

Enable for any throughput-bound send-heavy workload:

```rust
use monocoque::zmq::{PushSocket, SocketOptions};

let mut push = PushSocket::with_options(
    SocketOptions::default().with_write_coalescing(true),
);
push.connect("tcp://127.0.0.1:5555").await?;

// Messages accumulate in a 64 KB buffer; the kernel write fires when the
// buffer fills or when flush() is called.
for payload in &batch {
    push.send(payload.clone()).await?;
}
push.flush().await?;   // drain remaining bytes after the last send
```

Tuning the threshold:

```rust
SocketOptions::default()
    .with_write_coalescing(true)
    .with_write_coalesce_threshold(32_768)  // flush at 32 KB instead of 64 KB
```

Smaller thresholds lower the latency tail at the cost of slightly fewer messages
per syscall. The default 64 KB is optimal for sustained throughput on loopback.

### Coalescing on the connected sockets (0.4)

Before 0.4 only PUSH honored `with_write_coalescing`; DEALER, ROUTER, REQ, REP,
and PAIR ignored the flag and issued one kernel write per send regardless. 0.4
routes all of them through the same coalesce/vector/write path, so a batching
DEALER caller finally gets the syscall amortization PUSH already had. For a
1,000,000-message 64 B DEALER-to-ROUTER flow over TCP loopback (receiver-timed,
one-off probe harness):

| DEALER send path | throughput |
|---|---|
| coalescing off (the 0.3 behavior, one write per send) | 0.47 M msg/s |
| coalescing on (0.4 wires it in) | 18.0 M msg/s |

That is a ~38x improvement for batched DEALER workloads, and it is not
measurement-sensitive: the win is a deterministic drop in write submissions. The
same 20,000-message run under `strace` falls from 32,260 to 80 `io_uring_enter`
calls once coalescing is honored. The eager
(one-write-per-send) DEALER path is unchanged; this is purely what a caller gains
by opting into coalescing, which 0.3 did not deliver for these socket types.

---

### Why coalescing is opt-in

libzmq batches sends automatically because it spawns a background IO thread per
context. Every call to `zmq_send()` pushes the message into a lock-free queue,
and that IO thread drains the queue and calls `writev()` at its own pace. The
batching is a side effect of the thread scheduling, and libzmq users never think
about it.

monocoque works differently. There is no background IO thread. Everything runs on
a single `compio` runtime, and io_uring operations are submitted from the same
thread that called `send()`. This is one of the reasons monocoque has lower
latency for request-reply patterns: there is no cross-thread handoff between your
code and the IO work, and the async runtime can do something else while the write
completes.

The consequence is that batching requires a deliberate choice. When eager mode
(the default) is used, after `send()` returns the bytes are inside the kernel.
When coalescing is enabled, they may still be in a userspace buffer waiting for
the 64 KB threshold to fill. That difference matters in a few real situations:

- If your process crashes between `send()` and `flush()`, coalesced messages
  are lost. Eager-mode messages are already in the kernel buffer and will survive.
- If you are writing a request-reply loop, the send must reach the peer before
  you call `recv()`. With eager mode that is automatic; with coalescing you need
  `flush()` before the `recv()` or you will deadlock.
- If you are debugging a hang and asking "did my send go out?", eager mode makes
  the answer trivial. With coalescing, the answer is "maybe, check if flush() was
  called."

We kept coalescing opt-in because the right moment to call `flush()` depends on
your application logic, and getting it wrong silently is worse than having to
add one line. The Rust standard library makes the same call with `BufWriter`:
you opt into buffering and you own the `flush()` contract.

For a pure pipeline with no reply (PUSH/PULL), the pattern is simple: call
`flush()` once after your send loop and you are done. For request-reply (REQ/REP,
DEALER/ROUTER), stick with eager mode unless you have benchmarked a real
bottleneck and know exactly where to place each `flush()`.

---

## Vectored writes (large frames)

Large frames are written with a vectored write (`writev`) instead of being
copied into the userspace send buffer first. The frame header and the
refcounted `Bytes` body are handed to the kernel as a two-entry iovec, so the
body travels straight to the socket with no intermediate `memcpy`. The header is
built into a reused buffer and the iovec list is reused across calls, so the path
allocates nothing per message.

This is automatic, no API change is needed. The switch happens per message when
a frame body is at or above `vectored_write_threshold` (default 16 KiB). Small and
medium frames stay on the copy path, where a single contiguous `write` beats the
per-iovec bookkeeping.

The 16 KiB default was measured on loopback against 8 and 32 KiB: below the
threshold the copy plus one `write` is faster than a multi-segment `writev`; at
or above it, skipping the copy wins. At 16 KiB bodies it beats a 32 KiB
threshold by about half on coalesced throughput and 3% on round-trip latency,
and 8 KiB gains nothing more. Because the crossover depends on memory bandwidth and syscall cost, benchmark on
your own hardware and adjust:

```rust
use monocoque::zmq::{PushSocket, SocketOptions};

// Trigger vectored writes for smaller frames (only if it wins on your box).
let opts = SocketOptions::default().with_vectored_write_threshold(8192);

// Or disable vectored writes entirely (always copy into the send buffer).
let opts = SocketOptions::default().with_vectored_write_threshold(usize::MAX);
```

Notes and limits:

- Applies with and without write coalescing. With coalescing on, small frames
  are batched into the 64 KB buffer and large bodies still join the same write
  as their own iovec segments.
- Skipped for **CURVE**-encrypted connections: the cipher rewrites each body into
  a fresh buffer regardless, so there is no copy to avoid.
- Uses compio's `write_vectored_all` (`writev`). The further `IORING_OP_SEND_ZC`
  (kernel-buffer zero-copy) lever is not yet wired up.

---

## Borrowed receive

`recv()` returns a `MsgRef<'_>` that borrows the socket's receive slot, so a
receive loop allocates nothing per message and copies nothing: a frame of at
most 64 bytes lives inline in the `Msg`, and a longer one is a slice of the
buffer it was read into. Call `recv_owned()` (or `MsgRef::take`) only for a
message that has to outlive the next receive. A message already in the queue
is taken before any I/O work, so a receive never pays for reading.

```rust
use monocoque::zmq::PullSocket;

let mut pull = PullSocket::new();
pull.connect("tcp://127.0.0.1:5555").await?;
loop {
    let msg = pull.recv().await?;
    handle(msg.first().unwrap_or_default());
}
```

The allocation gate (`monocoque/tests/hotpath_alloc.rs`) holds every socket
type to that. On tokio it measures zero. compio's driver allocates its own op
record per read and write, and smol's reactor one per readiness wait; those
are the runtime's allocations, not the socket's.

### Atomics

`scripts/count_atomics.sh <backend>` counts the atomic (lock-prefixed)
instructions executed per message under callgrind, by function. 64 B messages
over TCP on one thread, every call counted:

| | compio | tokio | smol |
|---|---|---|---|
| borrowed receive of a queued message | 0 | 0 | 0 |
| a whole PUSH -> PULL message | 26.4 | 46.4 | 55.9 |

The second row covers sending, waking the reader and writer tasks, and the
runtime's own bookkeeping. About 18 of those per message are monocoque's task
wakeups (`ChildWaker`, `progress`); the rest are the runtime's (tokio's
readiness and scheduler, smol's reactor and parker) and two for the probe's
own `Bytes` payload clone on the send side.

---

## Worker pools (fan-out / fan-in)

Every socket is multi-peer, so a pool needs no special type. A `PushSocket`
that binds once (or connects to several endpoints) round-robins each `send`
across all its PULL peers, and a `PullSocket` fair-queues across all its PUSH
peers into one receive stream.

```rust
use monocoque::zmq::{PullSocket, PushSocket};

// Ventilator: workers connect to this endpoint as PULL sockets.
let mut ventilator = PushSocket::new();
ventilator.bind("tcp://127.0.0.1:5557").await?;

// Sink: workers connect to this endpoint as PUSH sockets.
let mut sink = PullSocket::new();
sink.bind("tcp://127.0.0.1:5558").await?;
```

Two notes on how this runs:

- All of a socket's peers are served on the one runtime thread that owns the
  socket. Each peer has its own reader loop, which decodes into that peer's
  ready queue and stops reading once the queue holds `recv_hwm` messages, so a
  lagging sink caps memory per worker instead of growing without bound.
  Spreading the readers across threads was measured and is a net loss at small
  messages: at ~10M msg/s the cost is dominated by cross-core cache and atomic
  `Bytes` refcount traffic, not decode. Threads only help large, decode-heavy
  messages, where the link bandwidth is the limit anyway.
- The PUSH side round-robins one message at a time. With coalescing each
  worker's queue flushes at the threshold, so the writes stay batched while all
  workers receive interleaved.

See `examples/pipeline_worker_pool.rs` and `examples/router_worker_pool.rs`.

---

## PUB/SUB broadcast coalescing

A `PubSocket` encodes each message once and appends the same refcounted wire
bytes to every matching subscriber's outbound queue, so the fan-out never copies
the payload. Each subscriber's writer drains everything queued for it as one
job: when a producer outpaces a subscriber and several broadcasts queue up while
a write is in flight, they go out together in the next vectored write,
amortizing the syscall cost across the burst.

This is automatic and requires no API change; it is what lets PUB fan-out keep
up under load rather than paying one syscall per message per subscriber.

---

## Buffer sizes

The read buffer size affects how many bytes fit before an extra read syscall is
needed. The default is a 32 KB read buffer. There is no write buffer setting:
the outbound buffer grows on demand and is reused. The read buffer is clamped to the 64 KB read slab, and floored at 64 bytes.

```rust
use monocoque_core::options::SocketOptions;

// Low-latency REQ/REP with small messages (< 1 KB)
let opts = SocketOptions::default().with_read_buffer_size(4096);

// Bulk transfers: widen the read batch to the full slab
let opts = SocketOptions::default().with_read_buffer_size(65_536);
```

Reach for a read buffer below the 32 KB default only to cut latency, never to
"tune" throughput.

**Rule of thumb:** set read buffer >= your 99th-percentile message size.

---

## High-water marks (HWM)

HWM limits the number of messages queued in memory, per peer and in each
direction, on every socket type and transport (inproc included). When a send
queue is full, sends wait (or fail with `Again`/`TimedOut` under a send timeout;
PUB drops for that subscriber). When a receive queue is full, the peer stops
being read, so the backpressure reaches the sender through the kernel. Lower HWM means lower memory footprint
and faster backpressure propagation; higher HWM means more buffering to absorb bursts.

```rust
let opts = SocketOptions::default()
    .with_send_hwm(500)    // halve the default send queue
    .with_recv_hwm(2000);  // double the default recv queue
```

Setting HWM to 0 disables the limit (unbounded queue, use with care), as in libzmq.

---

## io_uring tuning

This section applies to the default compio backend. The tokio and smol backends
do not use io_uring, so these knobs do not apply to them.

compio uses a shared io_uring ring per thread. On Linux >= 5.11 you get
the full benefit; older kernels fall back to thread-pool I/O.

- **SQ/CQ ring size**: Controlled by compio's runtime builder. Larger rings
  reduce submission overhead for high-connection-count servers.
- **SQPOLL**: Enables kernel-side submission polling, eliminating `io_uring_enter`
  syscalls at the cost of a dedicated CPU core. Useful only for
  sustained > 500 K msg/s workloads.
- **Fixed buffers**: reads go through a reused per-connection `BytesMut`, so
  the read path does not allocate on most reads. Registering those buffers as
  io_uring fixed buffers is future work.

### Spinning before sleeping

Sleeping in `io_uring_enter` and being woken by a completion costs more than a
whole round trip on a local socket. A bare io_uring ping-pong over a Unix
socket measured 8.3 µs per round trip when it slept for each completion and
4.0 µs when it polled the completion queue, against 6.3 µs for tokio's epoll.

So `monocoque::rt::LocalRuntime::block_on` on the compio backend polls for
completions for up to 20 µs, yielding the CPU between polls, before it
sleeps. A 64 B REQ/REP round trip over IPC went from 8.4 µs to 6.4 µs, level
with or ahead of tokio, and over TCP from 11.1 µs to 10.5 µs.

- Spinning is off when the thread can run on one CPU only (affinity mask or
  cgroup quota), where it would only delay the peer it waits for.
- After eight spins in a row that find nothing, the runtime sleeps straight
  away for the next 64 waits, so an idle socket does not keep a core busy.
- `MONOCOQUE_SPIN_US` sets the budget in microseconds; `0` turns it off.
- Code that runs on `#[compio::main]` or its own compio runtime instead of
  `LocalRuntime` does not spin.

---

## Thread model

Each compio runtime is **single-threaded**. For multi-core throughput, run one
runtime per core and connect sockets across runtimes:

```rust
for _ in 0..num_cpus::get() {
    std::thread::spawn(|| {
        compio::runtime::Runtime::new()
            .unwrap()
            .block_on(async { /* worker loop */ });
    });
}
```

For backend-agnostic code, `monocoque::rt::LocalRuntime::new()?.block_on(...)`
builds the right single-threaded runtime for whichever backend is enabled (a
compio runtime, a current-thread tokio runtime in a `LocalSet`, or a
single-threaded smol `LocalExecutor`).

Avoid sharing sockets across threads; monocoque sockets are `!Send`. This holds
on every backend: the tokio backend uses a current-thread runtime, not the
multi-threaded work-stealing scheduler, and the smol backend uses a
single-threaded `LocalExecutor`.

---

## Inproc vs TCP

`inproc://` transport skips the network stack entirely. `Msg` values are passed
via `flume` channels with no serialisation, bounded by the HWM in each
direction, within one thread or across threads.

| Transport | Typical latency | Zero-copy |
|---|---|---|
| inproc | < 1 µs | Arc clone |
| IPC (Unix socket) | 240-260 µs (per-connection) | io_uring |
| TCP loopback | 260-320 µs (per-connection) | io_uring |

Use inproc for inter-task communication within a process, IPC for same-host
cross-process, TCP for cross-host.

---

## Reconnection

Every connected endpoint is redialed automatically, with exponential backoff
and jitter (100 ms initial, unlimited max, unlimited attempts unless
`with_max_reconnect_attempts(Some(n))` is set).
For latency-sensitive workloads, tighten the backoff:

```rust
use std::time::Duration;

let opts = SocketOptions::default()
    .with_reconnect_ivl(Duration::from_millis(10))
    .with_reconnect_ivl_max(Duration::from_secs(1));
```

---

## Heartbeating

Enable ZMTP heartbeats on long-lived idle connections to detect dead peers
before the OS TCP keepalive fires (which can take minutes):

```rust
let opts = SocketOptions::default()
    .with_heartbeat_ivl(Duration::from_secs(20))
    .with_heartbeat_timeout(Duration::from_secs(5));
```

---

## TCP keepalive

TCP_NODELAY is on by default. For long-lived connections crossing NAT or
firewalls, enable OS-level keepalive:

```rust
let opts = SocketOptions::default()
    .with_tcp_keepalive(1)
    .with_tcp_keepalive_idle(60)
    .with_tcp_keepalive_intvl(10)
    .with_tcp_keepalive_cnt(5);
```

---

## Checklist

- Build with `--release`; debug builds are 5-10x slower
- Linux kernel >= 5.11 for full io_uring benefit
- Use `with_write_coalescing(true)` + `flush()` for small-message throughput
- Large frames use vectored writes automatically (>= 16 KiB body);
  tune `vectored_write_threshold` for your hardware
- Size the read buffer to match your 99th-percentile message size
- Use IPC instead of TCP loopback for co-located sockets (~2.4x throughput gain)
- Run `dhat` or `heaptrack` to catch `Bytes::copy_from_slice` in hot paths
- Verify io_uring is active: `/proc/$(pidof yourapp)/fdinfo` (look for `uring`)
