# Performance

## Benchmark results

All numbers measured on loopback against rust-zmq (FFI bindings to libzmq).
Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build, `rustc 1.96`.
Each benchmark runs sender and receiver on **separate OS threads** with separate
runtimes, so the numbers reflect real kernel TCP/IPC round-trips, not cooperative
task switching within a single runtime.

Both backends run the identical suite. compio uses io_uring, tokio uses epoll.
Pick the backend with a Cargo feature:

```bash
cargo bench --features zmq                                  # compio (default)
cargo bench --no-default-features --features runtime-tokio,zmq   # tokio
```

**What these numbers do and do not show.** These are single-connection loopback
microbenchmarks. On this workload the tokio/epoll backend is consistently a bit
faster than compio/io_uring: a single-flow ping-pong does not exercise io_uring's
strengths (batched submission, registered buffers, scaling across many concurrent
connections), so it just pays io_uring's per-operation submission overhead while
epoll plus a plain `write` stays leaner. io_uring's advantage shows up on real
network I/O and high connection counts, which these benches do not cover.
Benchmark on your own workload before picking a backend.

The rust-zmq control was stable across both backend runs (same latency and
throughput within noise), so the compio-vs-tokio differences below are real
measurements, not machine drift.

Throughput timer lives on the receiver side (starts before first recv, stops
after last recv). Latency timer wraps one send + recv + socket teardown per
iteration, after 1 000 warmup rounds on a fresh connection.

---

### Throughput - PUSH/PULL one-way, 10 000 messages

`eager` - default mode, one kernel write per `send()`.
`coalesced` - `with_write_coalescing(true)`, messages accumulate in a 64 KB
buffer flushed in one syscall; call `flush()` after the last send.

Eager mode (one syscall per message):

| Message size | compio | tokio | rust-zmq |
|---|---|---|---|
| 64 B | 339 K msg/s | 520 K msg/s | 1.33 M msg/s |
| 256 B | 344 K msg/s | 514 K msg/s | 1.09 M msg/s |
| 1 KB | 318 K msg/s | 410 K msg/s | 656 K msg/s |
| 4 KB | 292 K msg/s | 417 K msg/s | 328 K msg/s |
| 16 KB | 266 K msg/s | 317 K msg/s | 117 K msg/s |

Write coalescing (batched into 64 KB writes):

| Message size | compio | tokio | rust-zmq |
|---|---|---|---|
| 64 B | 9.2 M msg/s | **13.6 M msg/s** | 1.33 M msg/s |
| 256 B | 5.6 M msg/s | **9.8 M msg/s** | 1.09 M msg/s |
| 1 KB | 2.4 M msg/s | **5.3 M msg/s** | 656 K msg/s |
| 4 KB | 841 K msg/s | **1.74 M msg/s** | 328 K msg/s |
| 16 KB | 268 K msg/s | **473 K msg/s** | 117 K msg/s |

In eager mode both backends trail libzmq because each `send()` is one kernel
write (an io_uring SQE on compio, a `write` syscall on tokio) while libzmq
amortizes the syscall over an internal IO-thread batch. Write coalescing closes
that gap and then some: it batches ~970 x 64 B messages (or ~240 x 256 B) into
one `write_all()` call, eliminating the per-message kernel boundary crossing.
Coalesced, both backends beat libzmq by a wide margin: from ~7x (compio) and
~10x (tokio) at 64 B down to ~2.3x and ~4.0x at 16 KB. monocoque's coalescing is
explicit rather than a scheduling side effect, and achieves a higher batch ratio
with zero intermediate copies.

For **large** frames, eager mode no longer copies the body into the send buffer:
above `vectored_write_threshold` (default 32 KB) it writes the header and the
`Bytes` body as an iovec (`writev`), so the payload is never copied on its way to
the kernel. See [Vectored writes](#vectored-writes-large-frames) below.

---

### Throughput - DEALER/ROUTER batch API, 10 000 messages, batches of 100

The explicit `send_buffered() / flush()` API (used by the pipelined benchmark)
encodes N messages then issues one write, similar to coalescing but with manual
control over batch boundaries:

| Message size | compio | tokio |
|---|---|---|
| 64 B | 2.61 M msg/s (159 MiB/s) | 2.77 M msg/s (169 MiB/s) |
| 256 B | 2.04 M msg/s (497 MiB/s) | 2.14 M msg/s (522 MiB/s) |
| 1 KB | 1.14 M msg/s (1.09 GiB/s) | 1.56 M msg/s (1.49 GiB/s) |
| 4 KB | 372 K msg/s (1.42 GiB/s) | 619 K msg/s (2.36 GiB/s) |
| 16 KB | 97 K msg/s (1.49 GiB/s) | 118 K msg/s (1.80 GiB/s) |

---

### Latency - REQ/REP round-trip

Each iteration: 1 000 warmup rounds on a fresh connection (not measured), then
one send + recv + socket teardown. Because teardown is included (TCP FIN + thread
join), these numbers are higher than steady-state RTT on a persistent connection.
Both monocoque and zmq are measured identically.

| Message size | compio | tokio | rust-zmq |
|---|---|---|---|
| 64 B | 58 µs | 43 µs | 277 µs |
| 256 B | 51 µs | 42 µs | 265 µs |
| 1 KB | 61 µs | 48 µs | 261 µs |

Both backends are roughly 5x lower latency than libzmq (79% lower for compio,
84% for tokio at 64 B). The advantage over zmq is mainly that `drop(socket)` is
faster: the async socket cleanup completes synchronously within `block_on`, while
zmq's socket destructor involves thread synchronization. tokio edges compio on
this axis because the epoll wakeup path for a single-flow round-trip is shorter
than submitting and reaping an io_uring completion.

---

### IPC vs TCP loopback

Unix domain sockets (`UnixStream`) skip the IP stack entirely; on Linux they
share kernel buffers without copying.

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 59 µs | 51 µs | 56 µs |
| compio IPC | 57 µs | 67 µs | 60 µs |
| tokio TCP | 42 µs | 45 µs | 44 µs |
| tokio IPC | 55 µs | 54 µs | 57 µs |

On the latency axis IPC and TCP land within each other's noise band on both
backends. The per-iteration teardown (FIN/close plus thread join) dominates the
measurement and is similar for both transports, so IPC's lower per-message cost
does not show up here. The IPC advantage is on throughput, below.

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| compio TCP | 349 K msg/s | 346 K msg/s | 315 K msg/s |
| compio IPC | 724 K msg/s | 710 K msg/s | 683 K msg/s |
| tokio TCP | 520 K msg/s | 508 K msg/s | 463 K msg/s |
| tokio IPC | 1.47 M msg/s | 1.64 M msg/s | 1.39 M msg/s |

IPC throughput is ~2.1x TCP loopback on compio and ~2.8-3.2x on tokio, because
Unix sockets have lower per-syscall overhead and no TCP framing cost, and epoll's
leaner per-write path magnifies that gain.

---

### PUB/SUB patterns

Both run sender and subscribers on separate OS threads against the same peer
under test (monocoque vs rust-zmq).

**Fan-out** (single subscriber, 256 B messages):

| | Latency per message | Throughput | vs zmq |
|---|---|---|---|
| monocoque (compio) | 37 µs | 2.71 M msg/s | **3.1x faster** |
| monocoque (tokio) | 35 µs | 2.84 M msg/s | **3.3x faster** |
| rust-zmq | 115 µs | 870 K msg/s | |

**Topic filtering** (10% of messages match the subscription):

| | Latency per message | Throughput |
|---|---|---|
| monocoque (compio) | 4.8 µs | 20.7 M msg/s |
| monocoque (tokio) | 5.6 µs | 17.8 M msg/s |
| rust-zmq | 5.9-6.1 µs | 16-17 M msg/s |

On fan-out both backends lead libzmq by ~3x and are within noise of each other.
Topic filtering is a near tie with libzmq: the numbers move run to run (this is a
tight microbenchmark where a few hundred nanoseconds of filter cost dominates),
so treat it as parity rather than a decisive win either way.

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

On the coalesced PUSH/PULL throughput bench this lifts 64 B from 9.2 M to 11.3 M
msg/s on compio (about 1.23x) and from 13.6 M to 15.7 M on tokio, plus ~13% at
256 B; the gain tapers as messages grow and the path becomes bandwidth-bound. `recv()` and `try_recv()` are unchanged for callers
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
descriptor and is usable from any thread. On the **tokio** backend a `TcpStream` is
bound to the runtime that created it (the accepting runtime), so the worker's
writes only succeed while that runtime is still alive. In practice this is a
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

This section applies to the default compio backend. The tokio backend does not
use io_uring, so these knobs do not apply to it.

compio uses a shared io_uring ring per thread. On Linux >= 5.11 you get
the full benefit; older kernels fall back to thread-pool I/O.

- **SQ/CQ ring size**: Controlled by compio's runtime builder. Larger rings
  reduce submission overhead for high-connection-count servers.
- **SQPOLL**: Enables kernel-side submission polling, eliminating `io_uring_enter`
  syscalls at the cost of a dedicated CPU core. Useful only for
  sustained > 500 K msg/s workloads.
- **Fixed buffers**: compio's arena allocator already pins buffers for io_uring
  registered buffers. No extra work needed.

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
compio runtime, or a current-thread tokio runtime in a `LocalSet`).

Avoid sharing sockets across threads; monocoque sockets are `!Send`. This holds
on both backends: the tokio backend uses a current-thread runtime, not the
multi-threaded work-stealing scheduler.

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
