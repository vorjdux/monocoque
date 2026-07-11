# Performance

## Benchmark results

All numbers measured on loopback against rust-zmq (FFI bindings to libzmq).
Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build, `rustc 1.96`.
Each benchmark runs sender and receiver on **separate OS threads** with separate
runtimes, so the numbers reflect real kernel TCP/IPC round-trips, not cooperative
task switching within a single runtime.

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

All four columns were re-measured together for the 0.2 release on this box, on
the same corrected live-connection timer (see below). Latency is a fresh
steady-state run for all four columns.

Throughput timer lives on the receiver side and starts on a **live** connection
for both monocoque and rust-zmq: the receiver takes one warmup message before the
clock starts, so connection setup is excluded on both sides. (A prior bug started
the zmq receiver's timer before the sender connected and folded in a 5 ms startup
pause, which understated libzmq at small sizes; that is now fixed and both sides
are timed identically.) Latency is measured steady-state: the connection is
established once (plus 200 warmup rounds) outside the timer, then N back-to-back
round-trips are timed, with socket teardown and thread join happening after the
timer stops.

---

### Throughput - PUSH/PULL one-way, 10 000 messages

`eager` - default mode, one kernel write per `send()`.
`coalesced` - `with_write_coalescing(true)`, messages accumulate in a 64 KB
buffer flushed in one syscall; call `flush()` after the last send.

Eager mode (one syscall per message):

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 492 K msg/s | 493 K msg/s | 427 K msg/s | 4.58 M msg/s |
| 256 B | 488 K msg/s | 502 K msg/s | 417 K msg/s | 2.60 M msg/s |
| 1 KB | 451 K msg/s | 466 K msg/s | 383 K msg/s | 1.01 M msg/s |
| 4 KB | 378 K msg/s | 405 K msg/s | 295 K msg/s | 383 K msg/s |
| 16 KB | 357 K msg/s | 309 K msg/s | 296 K msg/s | 130 K msg/s |

Write coalescing (batched into 64 KB writes):

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 13.6 M msg/s | **17.1 M msg/s** | 13.2 M msg/s | 4.58 M msg/s |
| 256 B | 8.2 M msg/s | **12.0 M msg/s** | 8.5 M msg/s | 2.60 M msg/s |
| 1 KB | 3.5 M msg/s | **4.6 M msg/s** | 3.3 M msg/s | 1.01 M msg/s |
| 4 KB | 1.19 M msg/s | **1.60 M msg/s** | 1.10 M msg/s | 383 K msg/s |
| 16 KB | 370 K msg/s | **462 K msg/s** | 331 K msg/s | 130 K msg/s |

Eager mode is a **latency** tool, not a throughput one. Each `send()` puts that
message on the wire immediately with its own syscall, so you control exactly when
every message is delivered instead of waiting for a buffer to fill. That is what
you want for request/reply, RPC, and interactive protocols, where a message must
go out *now* and you shape the response per send. The throughput bench below runs
a firehose of one-way messages, which is the opposite of eager mode's purpose, so
read these numbers as "what happens if you eager-send a bulk stream," not as a
verdict on the mode.

On that firehose, libzmq's internal IO-thread batching dominates at small message
sizes: at 64 B it moves 4.58 M msg/s versus monocoque's 427-493 K (~9-11x),
because eager pays one kernel write per `send()` while libzmq amortizes many
messages per syscall. The gap closes as messages grow: around 4 KB the two are
near parity (libzmq 383 K vs monocoque 295-405 K), and by 16 KB monocoque eager
(296-357 K) is ~2.3-2.7x *faster* than libzmq (130 K), where the larger payload
amortizes the per-message syscall and vectored writes (`writev`, below) skip the
userspace copy. The takeaway: if you are streaming small messages in bulk, turn on
write coalescing; reach for eager when per-message delivery latency and control
matter more than aggregate rate.

Write coalescing batches ~970 x 64 B messages (or ~240 x 256 B) into one
`write_all()` call, eliminating the per-message kernel boundary crossing. Coalesced,
all three backends beat libzmq by ~2-4x across the range: at 64 B ~3.0x (compio),
~3.7x (tokio), ~2.9x (smol), and ~2.8x, ~3.6x, ~2.5x at 16 KB. tokio's epoll path
still leads on these single-flow loopback runs, but the compio 0.19 upgrade lifted
compio above smol (compio is io_uring's per-op submission overhead against epoll's
readiness model, now much narrower). monocoque's coalescing is explicit rather than a
scheduling side effect, and achieves a higher batch ratio with zero intermediate
copies.

For **large** frames, eager mode no longer copies the body into the send buffer:
above `vectored_write_threshold` (default 32 KB) it writes the header and the
`Bytes` body as an iovec (`writev`), so the payload is never copied on its way to
the kernel. See [Vectored writes](#vectored-writes-large-frames) below.

---

### Throughput - DEALER/ROUTER batch API, 10 000 messages, batches of 100

The explicit `send_buffered() / flush()` API (used by the pipelined benchmark)
encodes N messages then issues one write, similar to coalescing but with manual
control over batch boundaries:

| Message size | compio | tokio | smol |
|---|---|---|---|
| 64 B | 2.79 M msg/s (170 MiB/s) | 3.04 M msg/s (185 MiB/s) | 2.18 M msg/s (133 MiB/s) |
| 256 B | 2.17 M msg/s (530 MiB/s) | 2.43 M msg/s (593 MiB/s) | 1.81 M msg/s (442 MiB/s) |
| 1 KB | 1.15 M msg/s (1.09 GiB/s) | 1.59 M msg/s (1.51 GiB/s) | 1.24 M msg/s (1.18 GiB/s) |
| 4 KB | 338 K msg/s (1.29 GiB/s) | 612 K msg/s (2.33 GiB/s) | 473 K msg/s (1.80 GiB/s) |
| 16 KB | 92 K msg/s (1.40 GiB/s) | 122 K msg/s (1.85 GiB/s) | 112 K msg/s (1.70 GiB/s) |

---

### Latency - REQ/REP steady-state round-trip

The connection is established once (plus 200 warmup rounds) outside the timer;
then N back-to-back REQ/REP round-trips are timed on that persistent connection,
with socket teardown and thread join happening after the clock stops. These are
true steady-state round-trip times, not connection setup or teardown. Both
monocoque and zmq are measured identically.

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 9.4 µs | 10.5 µs | 12.9 µs | 35.2 µs |
| 256 B | 9.2 µs | 9.7 µs | 12.2 µs | 35.0 µs |
| 1 KB | 9.2 µs | 10.0 µs | 12.6 µs | 35.4 µs |

All three backends are ~2.7-3.8x lower round-trip latency than libzmq's ~35 µs:
compio and tokio are lowest (compio ~9.2 µs after the 0.19 upgrade), and smol
~12.6 µs (async-io's readiness
wakeup costs it a couple of microseconds over the other two). The
advantage over libzmq comes from the shorter userspace path on a single flow:
monocoque does the I/O inline on the same thread, with no handoff to a separate
IO thread on the send and recv path the way libzmq does. compio and tokio are
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
| compio TCP | 57 µs | 58 µs | 61 µs |
| compio IPC | 67 µs | 71 µs | 68 µs |
| tokio TCP | 51 µs | 52 µs | 55 µs |
| tokio IPC | 58 µs | 57 µs | 59 µs |
| smol TCP | 79 µs | 79 µs | 84 µs |
| smol IPC | 85 µs | 82 µs | 85 µs |

On the latency axis IPC and TCP land within each other's noise band on all three
backends. Unlike the steady-state latency table above, this bench still includes
per-iteration teardown (FIN/close plus thread join), which dominates the
measurement and is similar for both transports, so IPC's lower per-message cost
does not show up here. The IPC advantage is on throughput, below.

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 349 K msg/s | 349 K msg/s | 321 K msg/s |
| compio IPC | 717 K msg/s | 715 K msg/s | 686 K msg/s |
| tokio TCP | 519 K msg/s | 510 K msg/s | 486 K msg/s |
| tokio IPC | 1.69 M msg/s | 1.47 M msg/s | 1.44 M msg/s |
| smol TCP | 417 K msg/s | 418 K msg/s | 382 K msg/s |
| smol IPC | 1.40 M msg/s | 1.30 M msg/s | 1.13 M msg/s |

IPC throughput is ~3x TCP loopback on every backend (compio moved up to ~3.1x
with the 0.19 upgrade, from ~2.1x), because Unix sockets have lower per-syscall
overhead and no TCP framing cost.

---

### PUB/SUB patterns

Both run sender and subscribers on separate OS threads against the same peer
under test (monocoque vs rust-zmq).

**Fan-out** (single subscriber, 256 B messages):

| | Latency per message | Throughput | vs zmq |
|---|---|---|---|
| monocoque (compio) | 39 µs | 2.57 M msg/s | **3.0x faster** |
| monocoque (tokio) | 33 µs | 3.04 M msg/s | **3.5x faster** |
| monocoque (smol) | 36 µs | 2.78 M msg/s | **3.2x faster** |
| rust-zmq | 115 µs | 871 K msg/s | |

**Topic filtering** (10% of messages match the subscription):

| | Latency per message | Throughput |
|---|---|---|
| monocoque (compio) | 4.9 µs | 20.6 M msg/s |
| monocoque (tokio) | 4.9 µs | 20.3 M msg/s |
| monocoque (smol) | 5.6 µs | 17.8 M msg/s |
| rust-zmq | 5.9 µs | 16.9 M msg/s |

On fan-out all three backends lead libzmq by ~3x (3.0x compio, 3.5x tokio, 3.2x
smol) and are within noise of each other. Topic filtering is a near tie with
libzmq: the numbers move run to run (this is a tight microbenchmark where a few
hundred nanoseconds of filter cost dominates), so treat it as parity rather than
a decisive win either way.

---

## Write coalescing

Enable for any throughput-bound send-heavy workload:

```rust
use monocoque::zmq::{PushSocket, SocketOptions};

let mut push = PushSocket::connect_with_options(
    "127.0.0.1:5555",
    SocketOptions::default().with_write_coalescing(true),
).await?;

// Messages accumulate in a 64 KB buffer; the kernel write fires when the
// buffer fills or when flush() is called.
for msg in &batch {
    push.send(vec![msg.clone()]).await?;
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

In eager mode, large frames are written with a vectored write (`writev`) instead
of being copied into the userspace send buffer first. The frame header and the
refcounted `Bytes` body are handed to the kernel as a two-entry iovec, so the
body travels straight to the socket with no intermediate `memcpy`. The header is
built into a reused buffer and the iovec list is reused across calls, so the path
allocates nothing per message.

This is automatic, no API change is needed. The switch happens per message when
a frame body is at or above `vectored_write_threshold` (default 32 KB). Small and
medium frames stay on the copy path, where a single contiguous `write` beats the
per-iovec bookkeeping.

The 32 KB default is the measured crossover on loopback: below it the copy plus
one `write` is faster than a two-segment `writev`; at or above it, skipping the
copy wins. On this 4-core test box vectored writes are ~1.1-1.3x for 32 KB-1 MB
frames and ~30% *slower* at 16 KB, which is exactly why the threshold exists.
Because the crossover depends on memory bandwidth and syscall cost, benchmark on
your own hardware and adjust:

```rust
use monocoque::zmq::{PushSocket, SocketOptions};

// Trigger vectored writes for smaller frames (only if it wins on your box).
let opts = SocketOptions::default().with_vectored_write_threshold(16384);

// Or disable vectored writes entirely (always copy into the send buffer).
let opts = SocketOptions::default().with_vectored_write_threshold(usize::MAX);
```

Notes and limits:

- Applies in **eager** mode only. With write coalescing enabled, messages are
  batched into the 64 KB buffer instead (the two strategies serve different
  workloads: coalescing wins for small-message bursts, vectored writes for large
  frames).
- Skipped for **CURVE**-encrypted connections: the cipher rewrites each body into
  a fresh buffer regardless, so there is no copy to avoid.
- Uses compio's `write_vectored_all` (`writev`). The further `IORING_OP_SEND_ZC`
  (kernel-buffer zero-copy) lever is not yet wired up.

---

## Receive batching

`PullSocket::recv_batch()` is the receive-side counterpart to the send batch
API. It blocks until at least one message is available, then drains every
further message already decoded from the same kernel read, returning the whole
burst from a single `.await`. One `read` frequently delivers many small
messages; returning them together amortizes the per-await overhead that becomes
a real fraction of the budget at multi-million-msg/s rates.

```rust
use monocoque::zmq::PullSocket;

let mut pull = PullSocket::connect("127.0.0.1:5555").await?;
while let Some(batch) = pull.recv_batch().await? {
    for msg in batch {
        handle(msg);
    }
}
```

For finer control, `recv()` followed by `try_recv()` in a loop drains the same
buffer manually; `recv_batch()` just packages that pattern into one call.

In a loopback microbenchmark `recv_batch()` did **not** beat a tight `recv()`
loop (per-await scheduling is not the bottleneck there), so treat it as an
ergonomic convenience rather than a guaranteed speedup, and measure it against
`recv()` for your own workload before relying on it.

---

## Allocation-free receive (`recv_into`)

`recv()` allocates a fresh `Vec<Bytes>` for every message. At small message
sizes that allocation is the dominant per-message cost. `recv_into(&mut out)`
writes the frames into a caller-owned buffer instead, so a recv loop that reuses
one buffer allocates nothing per message. `try_recv_into` is the non-blocking
drain counterpart, for emptying everything decoded from one kernel read.

```rust
use bytes::Bytes;
use monocoque::zmq::PullSocket;

let mut pull = PullSocket::connect("127.0.0.1:5555").await?;
let mut buf: Vec<Bytes> = Vec::with_capacity(4);
while pull.recv_into(&mut buf).await? {
    handle(&buf);
    while pull.try_recv_into(&mut buf)? {
        handle(&buf);
    }
}
```

On the coalesced PUSH/PULL throughput bench this lifts 64 B from 11.8 M to 13.2 M
msg/s on compio (about 1.12x) and from 17.1 M to 18.7 M on tokio, plus a few
percent at 256 B; the gain tapers as messages grow and the path becomes
bandwidth-bound. `recv()` and `try_recv()` are unchanged for callers
that want an owned `Vec`. A runnable example lives at
`examples/recv_into_zero_alloc.rs`.

---

## Worker pools (fan-out / fan-in)

A single `PushSocket` or `PullSocket` owns one connection. For pool topologies,
`PushFanOut` binds once and round-robins each `send` across N PULL workers, and
`PullFanIn` binds once and merges N PUSH workers into one fair-queued stream.

Two notes from measuring these:

- The `PullFanIn` sink runs all its per-connection readers on **one** runtime and
  forwards each kernel-read burst across the merge channel in bounded-size chunks.
  That keeps it at the single-core decode ceiling (it matches single-stream PULL)
  while capping how many messages queue when the sink lags, so peak memory stays
  bounded instead of growing with worker count.
  Spreading the readers across threads was measured and is a net loss at small
  messages: at ~10M msg/s the cost is dominated by cross-core cache and atomic
  `Bytes` refcount traffic, not decode. Threads only help large, decode-heavy
  messages, where the link bandwidth is the limit anyway.
- The `PushFanOut` ventilator round-robins one message at a time. With coalescing
  each worker's buffer flushes at the threshold, so the writes stay batched while
  all workers receive interleaved. Handing each worker its whole share in one
  batched write instead serializes the pool and is markedly slower at large
  messages, so the per-message path is the one to use.

---

## PUB/SUB broadcast coalescing

The worker-pool `PubSocket` coalesces broadcasts automatically. When a producer
outpaces a worker and several broadcasts queue up, the worker drains them into a
batch and writes each subscriber its matching messages in a single vectored
write, amortizing the syscall cost across the burst. The plaintext fan-out stays
zero-copy: every subscriber's iovec entries are O(1) `Bytes` clones of the
shared per-message wire, never copies of the payload.

This is automatic and requires no API change; it is what lets PUB fan-out keep
up under load rather than paying one syscall per message per subscriber.

### Worker-pool PUB and runtime lifetime (tokio backend)

The worker-pool `PubSocket` accepts each subscriber on the caller's runtime, then
hands the connection to a worker thread that owns its own runtime and does the
actual broadcast writes. `send()` only queues to that worker and returns; the
socket write happens later, on the worker.

On the **compio** backend this just works: the accepted socket is a plain file
descriptor and is usable from any thread. The **smol** backend is likewise
unaffected: `async-io` registers streams with a process-wide global reactor, not
a per-runtime one, so a handed-off stream stays live regardless of which
executor accepted it. On the **tokio** backend a `TcpStream` is bound to the
runtime that created it (the accepting runtime), so the worker's writes only
succeed while that runtime is still alive. In practice this is a
non-issue for a normal long-running PUB server, whose accepting runtime stays up
for the process lifetime. It only bites a **short-lived publisher** that
broadcasts a burst and then lets its accepting runtime shut down immediately: the
tail of the broadcast can be dropped because the in-flight worker writes race the
teardown. If you drive a worker-pool `PubSocket` on tokio from a task that exits
right after sending, keep the accepting runtime alive until subscribers have
drained (for example, wait on a completion signal before returning from
`block_on`). The direct-stream sockets (PUSH/PULL, REQ/REP, DEALER/ROUTER) are
unaffected, since they never move a stream across runtimes.

---

## Buffer sizes

Read and write buffer sizes affect how many bytes fit before an extra read/write
syscall is needed. Default is 8 KB.

```rust
use monocoque_core::options::SocketOptions;

// Low-latency REQ/REP with small messages (< 1 KB)
let opts = SocketOptions::small();   // 4 KB buffers

// High-throughput PUSH/PULL with large messages (> 8 KB)
let opts = SocketOptions::large();   // 16 KB buffers

// Custom
let opts = SocketOptions::default()
    .with_buffer_sizes(32_768, 32_768);
```

**Rule of thumb:** set read buffer >= your 99th-percentile message size.

---

## High-water marks (HWM)

HWM limits the number of messages queued in memory. When the queue is full,
sends block (or drop on PUB sockets). Lower HWM means lower memory footprint
and faster backpressure propagation; higher HWM means more buffering to absorb bursts.

```rust
let opts = SocketOptions::default()
    .with_send_hwm(500)    // halve the default send queue
    .with_recv_hwm(2000);  // double the default recv queue
```

Setting HWM to 0 disables the limit (unbounded queue, use with care).

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
- **Fixed buffers**: reads go through a reused per-socket `BytesMut` slab
  (`core::io::take_read_buffer`), so the read path already avoids a fresh
  allocation on most reads. Registering those slabs as io_uring fixed buffers is
  future work; see the roadmap.

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

`inproc://` transport skips the network stack entirely. Messages are passed
via `flume` channels with no serialisation.

| Transport | Typical latency | Zero-copy |
|---|---|---|
| inproc | < 1 µs | Arc clone |
| IPC (Unix socket) | 240-260 µs (per-connection) | io_uring |
| TCP loopback | 260-320 µs (per-connection) | io_uring |

Use inproc for inter-task communication within a process, IPC for same-host
cross-process, TCP for cross-host.

---

## Reconnection

Reconnection uses exponential backoff by default (100 ms initial, unlimited max).
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
- Large frames use vectored writes automatically (eager mode, >= 32 KB body);
  tune `vectored_write_threshold` for your hardware
- Size read/write buffers to match your 99th-percentile message size
- Use IPC instead of TCP loopback for co-located sockets (~2.4x throughput gain)
- Run `dhat` or `heaptrack` to catch `Bytes::copy_from_slice` in hot paths
- Verify io_uring is active: `/proc/$(pidof yourapp)/fdinfo` (look for `uring`)
