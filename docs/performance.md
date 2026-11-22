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

- `monocoque/benches/instr_hotpath.rs` counts instructions on the IO-free hot path under
  callgrind; CI fails a run more than 5% over the committed
  `docs/perf/instr-baseline.json`, or with a bench on only one side
  (`scripts/check_instr_baseline.py --update` rewrites it after an intended
  change)
- `monocoque/tests/hotpath_alloc.rs` asserts the per-message allocation count
- `monocoque/tests/socket_footprint_bound.rs` bounds idle per-socket resident memory
- `docs/perf/baselines/` holds per-commit snapshots for reference; no gate
  reads them

The rest of this document is tuning guidance: which knobs exist, what each one
trades, and when to reach for it.

## Local benchmark suite

Hardware for the local figures quoted below: Intel Core i7-1355U (12 threads),
Linux 7.0, release build, `rustc 1.98`. Each benchmark runs sender and receiver
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
microbenchmarks. On this workload no backend leads everywhere: compio/io_uring
is ahead on coalesced throughput from 1 KB up, where a write is in flight while
the next batch queues, and tokio/epoll has the shortest round trip by a few
tenths of a microsecond, where a readiness wakeup plus a plain `read` is
leaner than a submission and a completion. A
single flow does not exercise io_uring's other strengths (batched submission,
registered buffers, scaling across many concurrent connections), which show up
on real network I/O and high connection counts that these benches do not cover.
Benchmark on your own workload before picking a backend.

**These are 1.0 numbers.** All columns were measured together in one run on this
box on a quiet machine. The two-thread benches (throughput, latency, IPC vs TCP,
pipelined) ran pinned to two performance cores (`taskset -c 0,2`); the
multi-thread benches (PUB/SUB patterns) ran unpinned.

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
| 64 B | 546 K msg/s | 554 K msg/s | **582 K msg/s** |
| 256 B | 539 K msg/s | 542 K msg/s | **561 K msg/s** |
| 1 KB | 423 K msg/s | 466 K msg/s | **508 K msg/s** |
| 4 KB | 382 K msg/s | 418 K msg/s | **445 K msg/s** |
| 16 KB | 281 K msg/s | 294 K msg/s | **311 K msg/s** |

Write coalescing (batched into 64 KB writes):

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 32.5 M msg/s | **33.3 M msg/s** | 29.2 M msg/s |
| 256 B | 16.5 M msg/s | 16.1 M msg/s | **16.8 M msg/s** |
| 1 KB | **7.42 M msg/s** | 6.86 M msg/s | 6.84 M msg/s |
| 4 KB | **2.19 M msg/s** | 2.06 M msg/s | 2.05 M msg/s |
| 16 KB | **611 K msg/s** | 541 K msg/s | 511 K msg/s |

Eager mode is a **latency** tool, not a throughput one. Each `send()` puts that
message on the wire immediately with its own syscall, so you control exactly when
every message is delivered instead of waiting for a buffer to fill. That is what
you want for request/reply, RPC, and interactive protocols, where a message must
go out *now* and you shape the response per send. The throughput bench below runs
a firehose of one-way messages, which is the opposite of eager mode's purpose, so
read these numbers as "what happens if you eager-send a bulk stream," not as a
verdict on the mode.

The eager-versus-coalesced gap is the point of these two tables. At 64 B,
coalescing moves 50 to 60x more messages, because eager pays one kernel write
per `send()` while coalescing amortizes hundreds of messages per syscall. The gap
narrows as messages grow: by 16 KB a single payload nearly fills a write on its
own, and the two modes converge. The takeaway: if you are streaming small
messages in bulk, turn on write coalescing; reach for eager when per-message
delivery latency and control matter more than aggregate rate.

For how either mode compares against other implementations, see
[zmq-arena](https://vorjdux.github.io/zmq-arena/).

Write coalescing batches ~990 x 64 B messages (or ~247 x 256 B) into one
write, eliminating the per-message kernel boundary crossing. The backends are
close at 64 B and 256 B, and compio leads from 1 KB up: its write is in flight
while the next batch queues, so its batches run larger than the threshold, and
on tokio and smol each batch is one write of one TCP segment (see [Write coalescing](#write-coalescing)). Coalescing here is
explicit rather than a scheduling side effect, and achieves a high batch ratio
with zero intermediate copies.

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
| 64 B | 5.01 M msg/s (306 MiB/s) | **5.07 M msg/s** (310 MiB/s) | 4.71 M msg/s (287 MiB/s) |
| 256 B | 3.54 M msg/s (863 MiB/s) | **3.66 M msg/s** (894 MiB/s) | 3.42 M msg/s (836 MiB/s) |
| 1 KB | 2.48 M msg/s (2.37 GiB/s) | **2.74 M msg/s** (2.61 GiB/s) | 2.47 M msg/s (2.35 GiB/s) |
| 4 KB | 953 K msg/s (3.64 GiB/s) | **993 K msg/s** (3.79 GiB/s) | 983 K msg/s (3.75 GiB/s) |
| 16 KB | 278 K msg/s (4.24 GiB/s) | 269 K msg/s (4.11 GiB/s) | **292 K msg/s** (4.46 GiB/s) |

---

### Latency - REQ/REP steady-state round-trip

The connection is established once (plus 200 warmup rounds) outside the timer;
then N back-to-back REQ/REP round-trips are timed on that persistent connection,
with socket teardown and thread join happening after the clock stops. These are
true steady-state round-trip times, not connection setup or teardown.

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 5.6 µs | **5.3 µs** | 6.9 µs |
| 256 B | 5.6 µs | **5.5 µs** | 7.5 µs |
| 1 KB | 6.0 µs | **5.7 µs** | 8.0 µs |

The same round trip over a Unix socket:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 3.2 µs | **3.0 µs** | 4.6 µs |
| 256 B | 3.3 µs | **3.1 µs** | 4.5 µs |
| 1 KB | 3.6 µs | **3.4 µs** | 4.7 µs |

tokio is lowest on both transports, with compio within 0.3 µs of it and smol
1.3 to 2.3 µs behind. The short round trip comes from doing the I/O
inline on the same thread, with no handoff to a separate IO thread on the send
and recv path, and from looking for the reply before sleeping (see
[Spinning before sleeping](#spinning-before-sleeping)). For how this compares
against other implementations, see
[zmq-arena](https://vorjdux.github.io/zmq-arena/). On real network I/O and
high connection counts io_uring's batching counts for more than it does on one
loopback flow.

---

### IPC vs TCP loopback

Unix domain sockets (`UnixStream`) skip the IP stack entirely; on Linux they
share kernel buffers without copying.

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 25 µs | 25 µs | 30 µs |
| compio IPC | 32 µs | 33 µs | 32 µs |
| tokio TCP | 21 µs | 21 µs | 26 µs |
| tokio IPC | 24 µs | 23 µs | 23 µs |
| smol TCP | 26 µs | 27 µs | 32 µs |
| smol IPC | 25 µs | 25 µs | 27 µs |

Unlike the steady-state latency tables above, this bench still includes
per-iteration teardown (socket close and server thread join; connect and
warmup are outside the timer), which dominates the measurement: the round trip itself is a few microseconds of each
figure. Read it as the cost of a short-lived connection, and take IPC's per-message advantage from the steady-state table
and from the throughput below.

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 526 K msg/s | 525 K msg/s | 421 K msg/s |
| compio IPC | 2.41 M msg/s | 2.20 M msg/s | 2.01 M msg/s |
| tokio TCP | 557 K msg/s | 534 K msg/s | 459 K msg/s |
| tokio IPC | 2.26 M msg/s | 2.08 M msg/s | 2.08 M msg/s |
| smol TCP | 584 K msg/s | 556 K msg/s | 491 K msg/s |
| smol IPC | 2.19 M msg/s | 2.03 M msg/s | 1.85 M msg/s |

IPC throughput is about 4x TCP loopback on every backend (3.7x to 4.8x across
the cells above), because Unix sockets
have lower per-syscall overhead and no TCP framing cost.

---

### PUB/SUB patterns

Sender and subscribers run on separate OS threads.

Each case runs with an eager publisher, which pays one send per subscriber per
message, and with a coalescing one (see
[PUB/SUB broadcast coalescing](#pubsub-broadcast-coalescing)). Throughput
counts deliveries, so four subscribers each receiving a message count four.

**Fan-out** (256 B messages, batches of 100):

| | Eager, 1 sub | Eager, 4 subs | Coalesced, 1 sub | Coalesced, 4 subs |
|---|---|---|---|---|
| monocoque (compio) | 426 K msg/s | 242 K msg/s | 14.6 M msg/s | 18.3 M msg/s |
| monocoque (tokio) | 450 K msg/s | 314 K msg/s | 14.4 M msg/s | 22.1 M msg/s |
| monocoque (smol) | 482 K msg/s | 326 K msg/s | 14.0 M msg/s | 17.9 M msg/s |

**Topic filtering** (10% of messages match the subscription):

| | Eager | Coalesced |
|---|---|---|
| monocoque (compio) | 3.72 M msg/s | 31.2 M msg/s |
| monocoque (tokio) | 4.00 M msg/s | 30.7 M msg/s |
| monocoque (smol) | 4.19 M msg/s | 28.9 M msg/s |

An eager publisher is the low-latency choice and a coalescing one the
high-throughput choice, by a factor of 30 or more on fan-out. The eager
four-subscriber column moves a lot from run to run. Topic filtering is
a tight microbenchmark where a few hundred nanoseconds of filter cost dominates,
so its numbers move run to run; read them as an order of magnitude rather than a
ranking. Cross-implementation PUB/SUB results, including runs at 4 and 32
subscribers, are in [zmq-arena](https://vorjdux.github.io/zmq-arena/).

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

Smaller thresholds lower the latency tail at the cost of fewer messages per
syscall. The default 64 KB is one TCP segment on loopback.

On TCP a batch released by the threshold is written as whole segments. A
batch is rarely a whole number of them: 64 KB of 256 B frames comes to 65 720
bytes against a loopback segment of 65 483, and the kernel sends the odd 237
bytes as a segment of their own, a second pass through the stack on the
sender for almost nothing. The writer asks the socket for its segment size
(`TCP_MAXSEG`) and sends the bytes left over with the next batch while the
sender keeps sending. They never wait for a `flush()`: once the sending task
yields to the runtime the socket's driver writes them, with anything queued
since, so a sender that crosses the threshold and then waits for a reply does
not strand part of a message. `flush()`, a close and a queue at its HWM write
everything. Measured
on a PUSH/PULL pair of 256 B frames on smol, one development host:

| Bytes per write | Segments | Throughput |
|---|---|---|
| 32 595 | half of one | 14.1 M msg/s |
| 65 720 (before) | one and a short one | 15.1 M msg/s |
| 65 483 (now) | one | 15.9 M msg/s |
| 130 910 (128 KB threshold) | two | 17.4 M msg/s |

The last row is why a larger threshold is worth trying for a bulk sender: each
write carries two segments for the same number of syscalls per byte, at the
price of up to 128 KB waiting in user space.

### What is never held

Coalescing holds the application's messages only. What the protocol itself
sends is written at once, and takes any held messages with it:

- heartbeat PINGs and PONGs, so a batch that is slow to fill cannot time a
  connection out;
- subscriptions, unsubscriptions and other control messages, so a SUB that
  subscribes on a coalescing socket starts receiving without a `flush()`;
- the messages a new peer is greeted with (a SUB's stored subscriptions, an
  XPUB's welcome message);
- a STREAM close (`[identity, ""]`), which writes what was queued for that
  connection before it closes it.

### Coalescing on every socket type

`with_write_coalescing` applies to every socket type that sends. PUSH, DEALER,
ROUTER, REQ, REP, PAIR and the publishers all queue through the same
coalesce/vector/write path, so a DEALER that sends in batches gets the same
syscall amortization as a PUSH.

The figures below are a 0.4 measurement from a one-off probe harness on the
compio backend, not part of the 1.0 suite at the top of this document; they
are kept because they show the mechanism. A 1,000,000-message 64 B
DEALER-to-ROUTER flow over TCP loopback, receiver-timed:

| DEALER send path (0.4, probe harness) | throughput |
|---|---|
| coalescing off (one write per send) | 0.47 M msg/s |
| coalescing on | 18.0 M msg/s |

That is about 38x for a batched DEALER workload, and it does not depend on the
machine: the gain is a drop in write submissions. A 20,000-message run of the
same probe under `strace` made 32,260 `io_uring_enter` calls with coalescing
off and 80 with it on. For 1.0 figures on all three backends see the PUSH/PULL
and DEALER/ROUTER tables in the [local benchmark suite](#local-benchmark-suite).

---

### Why coalescing is opt-in

libzmq batches sends automatically because it spawns a background IO thread per
context. Every call to `zmq_send()` pushes the message into a lock-free queue,
and that IO thread drains the queue and calls `writev()` at its own pace. The
batching is a side effect of the thread scheduling, and libzmq users never think
about it.

monocoque works differently. There is no background IO thread. A socket does its
I/O on the single-threaded runtime that owns it, whichever backend that is, and
the write is issued from the same thread that called `send()`: an io_uring
submission on compio, a `write` on tokio and smol. This is one of the reasons monocoque has lower
latency for request-reply patterns: there is no cross-thread handoff between your
code and the IO work, and the async runtime can do something else while the write
completes.

The consequence is that batching requires a deliberate choice. When eager mode
(the default) is used, after `send()` returns the bytes are inside the kernel.
When coalescing is enabled, they may still be in a userspace buffer waiting for
the 64 KB threshold to fill. The one exception in eager mode is a peer that
stopped reading: once it has taken under 16 KiB in a `write_stall` window
(50 ms by default; noticing takes up to two windows), sends stop waiting on it
until it reads again, so it cannot hold up the socket's other peers. A peer is
judged over a whole window of waiting, counted across sends, so a
`send_timeout` shorter than the window neither hides a peer that stopped
reading nor reports one that reads in bursts and acknowledges nothing for a
moment between them. A slow peer that keeps reading is waited for. The
socket's monitor reports `PeerStalled`, `with_write_stall(None)` always waits,
and `close()` waits for everything up to `linger`, so call it before a sender
exits.

This protection is Linux only in full. On Linux a peer's progress is what it
acknowledged (`TIOCOUTQ`); elsewhere it is what was written into the local
socket, which a peer that stopped reading still accepts until both kernel
buffers are full. So on macOS and the BSDs a slow reader can still lose
messages left in kernel buffers when the sender exits without `close()`.

That difference matters in a few real situations:

- If your process crashes between `send()` and `flush()`, coalesced messages
  are lost. Eager-mode messages are already in the kernel buffer and will
  survive, except what is queued for a stalled peer.
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
- Uses `write_vectored_all` (`writev`) on every backend. The further `IORING_OP_SEND_ZC`
  (kernel-buffer zero-copy) lever is not yet wired up.
- One vectored write carries at most 128 KiB, or 32 KiB for an eager peer on
  tokio and smol. A write the socket buffer cannot take whole leaves the
  writer waiting for room and the reader idle once it has drained the rest; an
  uncapped job of 64 segments was about 512 KiB at 16 KiB frames.
- On compio, a frame under 64 KiB that does not fit the read buffer gets room
  for several like it, so one read brings in several large frames. The
  readiness backends lost about as much from this as compio gained, and keep a
  buffer per frame. tokio and smol read in full while a stream keeps filling
  their reads; on tokio only a read given a whole `read_buffer_size` counts as
  filled, since an eager message fills the tail a buffer had left by chance.
  smol also finishes a frame that straddles a read from the next ordinary
  read; on tokio that copy cost an eager 16 KiB stream more than the read it
  saved.

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
| a whole PUSH -> PULL message | 16.3 | 29.3 | 34 to 46 |

The second row covers sending, waking the reader and writer tasks, and the
runtime's own bookkeeping. About 9 of those per message are monocoque's task
wakeups (`ChildWaker`, `make_progress`, `park_user`); the rest are the
runtime's (compio's driver, tokio's readiness and scheduler, smol's reactor
and parker) and one for the probe's own `Bytes` payload clone on the send
side. smol's count moves from run to run with how often its reactor thread
parks. The probe runs with the spin before sleeping turned off, which would
add polling that is not per-message work.

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

See `monocoque/examples/pipeline_worker_pool.rs` and
`monocoque/examples/router_worker_pool.rs`.

---

## PUB/SUB broadcast coalescing

A `PubSocket` encodes each message once and appends the same refcounted wire
bytes to every matching subscriber's outbound queue, so the fan-out never copies
the payload. Each subscriber's writer drains everything queued for it as one
job, so broadcasts that queued up while a write was in flight go out together.

A PUB is eager by default, like every socket: `send()` runs each matching
subscriber's writer before it returns, so the message is in every kernel buffer
and the latency is the lowest available. That costs one send per subscriber per
message, and with a hundred subscribers a hundred sends per message. For
throughput, turn on write coalescing, which batches each subscriber's messages
into one write:

```rust
use monocoque::zmq::{PubSocket, SocketOptions};

let mut publisher =
    PubSocket::with_options(SocketOptions::default().with_write_coalescing(true));
```

0.4's PUB batched like this on its own, on worker threads, so compare 0.4
against the coalesced cases (`pubsub_fanout_coalesced`,
`topic_filtering_coalesced`, and the `_coalesced` slow-subscriber case), not the
eager ones.

Publishing only reads the message. `send_ref(&msg)` publishes one the caller
keeps, so a message published repeatedly needs no clone per send: for a
two-frame topic message the clone cost more than the publish. XPUB and RADIO
have it too. A topic or group no subscriber wants is dropped before any fan-out
work.

---

## Buffer sizes

The read buffer size affects how many bytes fit before an extra read syscall is
needed. The default is a 32 KB read buffer. There is no write buffer setting:
the outbound buffer grows on demand and is reused. The read buffer is clamped to 64 KB and floored at 64 bytes.

The setting is the size a connection starts at. While reads keep filling the
buffer, a busy connection doubles its read size up to 64 KB, and halves it back
toward the setting once reads come back well short. The buffer is also
refreshed once less than a quarter of a read is left in it: frames are split off
its front as they are delivered, and reading into whatever tail remained let
reads shrink to a few hundred bytes. Two-frame messages on io_uring, where every
read is a submission, ran at a sixth of single-frame throughput that way.

```rust
use monocoque::zmq::SocketOptions;

// Low-latency REQ/REP with small messages (< 1 KB)
let opts = SocketOptions::default().with_read_buffer_size(4096);

// Bulk transfers: widen the read batch to the 64 KiB maximum
let opts = SocketOptions::default().with_read_buffer_size(65_536);
```

Reach for a read buffer below the 32 KB default only to cut latency, never to
"tune" throughput.

**Rule of thumb:** when your 99th-percentile message is under 64 KB, set the
read buffer at or above its size so that such a message arrives in one read. A
larger message takes several reads whatever the setting, since 64 KB is the
most one read returns; use the maximum for those.

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

The ring settings apply to the default compio backend; the tokio and smol
backends do not use io_uring. Spinning before sleeping, below, applies to all
three.

compio drives one io_uring per runtime, and so one per thread; every socket on
that thread shares it.

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
completions for up to 50 µs, yielding the CPU between polls, before it
sleeps. The budget is sized for a peer that answers a batch: a ROUTER echoing
100 messages of 256 B takes 22 to 30 µs to reply on an EPYC 7232P, and a
20 µs spin gave up just before the reply came (6.63 ms per 100 batches at
20 µs, 5.80 ms at 40 µs and above). A 64 B REQ/REP round trip over IPC went
from 8.4 µs to 6.4 µs with the spin, level with or ahead of tokio, and over
TCP from 11.1 µs to 10.5 µs.

- Spinning is off when the thread can run on one CPU only (affinity mask or
  cgroup quota), where it would only delay the peer it waits for.
- After eight spins in a row that find nothing, the runtime sleeps straight
  away for the next 64 waits, so an idle socket does not keep a core busy.
- On tokio and smol, a spin ends as soon as another socket on the same thread
  comes to wait, and stays off for the next 1024 waits. A spin keeps its
  thread busy, and a busy thread does not get round to its reactor, so two
  sockets talking to each other on one thread each kept the other from being
  told its bytes had arrived. A DEALER and a ROUTER echoing on one thread
  took 611 µs per thousand 64 B messages on smol with the spin and 219 µs
  with this rule (237 µs and 185 µs on tokio). Sockets on threads of their
  own spin as before.
- A spin that finds its message can still be the slower way. A spinning
  receiver keeps entering the kernel, and when it shares no L3 cache with its
  peer that slows the peer down. On an EPYC 7232P (four L3 groups of two
  cores) a tokio REQ/REP round trip of 64 B took 13.5 µs spinning and 20.9 µs
  sleeping with both threads under one L3, and 29.8 µs spinning and 21.8 µs
  sleeping under two. The library does not detect this. If you can choose
  where threads run, put two that talk to each other on cores that share an
  L3 cache (`lscpu -e`, the L3 column); if you cannot, measure with the spin
  turned off.
- `LocalRuntime::new()?.with_spin(duration)` sets the budget for everything
  that runtime runs, on every backend. On tokio and smol
  `SocketOptions::default().with_recv_spin(duration)` sets it for one socket,
  ahead of the runtime's, and is the way to set it under `#[tokio::main]` or
  another runtime of your own. `Duration::ZERO` turns it off.
- Code that runs on `#[compio::main]` or its own compio runtime instead of
  `LocalRuntime` does not spin.
- On tokio and smol a receive about to sleep yields to the runtime instead,
  under the same rules, for 30 µs on tokio and 100 µs on smol. smol wakes a
  sleeping thread through one reactor shared by the whole process, so a sleep
  costs more there: with one sender feeding four workers, each worker slept
  between its batches at 20 µs and the pool ran at half of 0.4's rate (5.4 M
  against 9.8 M msg/s at 64 B), and at 100 µs it passed it (10.2 M), for about
  a quarter more CPU time over the run. Other tasks on that runtime keep
  running meanwhile, and a fast sender no longer pays to wake the receiver
  inside every `send`. On a development host a 64 B REQ/REP round trip on TCP
  went from about 11 µs to about 5.3 µs on tokio, and from about 16 µs to about
  7 µs on smol.
- Spinning costs CPU while traffic flows: on tokio, with 100 000 messages per
  connection, about 5% to 15% more CPU per message than with it off. With
  a zero budget a receive sleeps and is woken per message again.

---

## Thread model

Each runtime is **single-threaded**, on every backend. For multi-core
throughput, run one runtime per core and connect sockets across runtimes:

```rust
use monocoque::rt::LocalRuntime;

for _ in 0..num_cpus::get() {
    std::thread::spawn(|| {
        LocalRuntime::new()
            .unwrap()
            .block_on(async { /* worker loop */ });
    });
}
```

`LocalRuntime` builds the right single-threaded runtime for whichever backend
is enabled (a compio runtime, a current-thread tokio runtime in a `LocalSet`,
or a single-threaded smol `LocalExecutor`). Use it rather than
`compio::runtime::Runtime::new()` or `#[compio::main]`: those work, but only
`LocalRuntime` spins before sleeping on compio (see
[Spinning before sleeping](#spinning-before-sleeping)), and the latency numbers
in this document were measured with the spin.

Avoid sharing sockets across threads; monocoque sockets are `!Send`. This holds
on every backend: the tokio backend uses a current-thread runtime, not the
multi-threaded work-stealing scheduler, and the smol backend uses a
single-threaded `LocalExecutor`.

---

## Inproc vs TCP

`inproc://` transport skips the network stack entirely: `Msg` values move
between the sockets with no encoding, bounded by the HWM in each direction.
Two sockets on one thread share a queue that uses no atomics; sockets on
different threads use a `flume` channel.

| Transport | What a message costs | Copies |
|---|---|---|
| inproc | a queue push and pop, no syscall | none: the `Msg` itself moves |
| IPC (Unix socket) | ZMTP framing and a kernel round trip | body to and from the kernel |
| TCP loopback | the same, plus the TCP stack | body to and from the kernel |

For measured numbers see the zmq-arena runs linked from the README; its inproc
cell runs both ends in one process across two threads.

Use inproc for inter-task communication within a process, IPC for same-host
cross-process, TCP for cross-host.

---

## Reconnection

Every connected endpoint is redialed automatically, with unlimited attempts
unless `with_max_reconnect_attempts(Some(n))` is set. By default the interval
is constant: `reconnect_ivl` is 100 ms and `reconnect_ivl_max` is zero, which
means no backoff, as in libzmq. Each wait is jittered to between half the
interval and the whole of it. Setting `reconnect_ivl_max` above `reconnect_ivl`
turns on exponential backoff: the interval doubles after each failed dial up to
the maximum, and resets once a peer comes up.

For latency-sensitive workloads, redial sooner and cap the backoff:

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

A PING goes out when nothing has been received from a peer for
`heartbeat_ivl`, and the connection is closed when the peer then stays silent
for `heartbeat_timeout` more (the interval again when no timeout is set).
Heartbeats are written at once on a coalescing socket too.

By default a PING carries no TTL, as in libzmq, so the peer applies only its
own timeouts. `with_heartbeat_ttl` adds one: the peer then closes the
connection when it hears nothing from this socket for that long. With a TTL
set, a PING is sent every `heartbeat_ivl` whether or not the connection is
idle, because the peer counts from the last PING. Set the TTL comfortably
above the interval (two or three times it); at or below the interval the
peer's timer can run out between two PINGs.

```rust
let opts = SocketOptions::default()
    .with_heartbeat_ivl(Duration::from_secs(5))
    .with_heartbeat_ttl(Duration::from_secs(15));
```

---

## TCP keepalive

TCP_NODELAY is always on; there is no option to turn it off. For long-lived connections crossing NAT or
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
- Size the read buffer to your 99th-percentile message size, up to the 64 KB
  maximum
- Use IPC instead of TCP loopback for co-located sockets (about 4x the throughput)
- Run `dhat` or `heaptrack` to catch `Bytes::copy_from_slice` in hot paths
- Verify io_uring is active: `/proc/$(pidof yourapp)/fdinfo` (look for `uring`)
