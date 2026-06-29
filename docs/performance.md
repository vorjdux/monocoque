# Performance

## Benchmark results

All numbers measured on loopback against rust-zmq (FFI bindings to libzmq).
Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, release build, `rustc 1.96`.
Each benchmark runs sender and receiver on **separate OS threads** with separate
`compio` runtimes, so the numbers reflect real kernel TCP/IPC round-trips, not
cooperative task switching within a single runtime.

Throughput timer lives on the receiver side (starts before first recv, stops
after last recv). Latency timer wraps one send + recv + socket teardown per
iteration, after 1 000 warmup rounds on a fresh connection.

Run: `cargo bench --features zmq` from the `monocoque/` directory.

---

### Throughput - PUSH/PULL one-way, 10 000 messages

`monocoque (eager)` - default mode, one kernel write per `send()`.
`monocoque (coalesced)` - `with_write_coalescing(true)`, messages accumulate in
a 64 KB buffer flushed in one syscall; call `flush()` after the last send.

| Message size | monocoque eager | monocoque coalesced | rust-zmq | coalesced vs zmq |
|---|---|---|---|---|
| 64 B | 339 K msg/s | **9.2 M msg/s** | 1.32 M msg/s | **7.0x faster** |
| 256 B | 343 K msg/s | **5.5 M msg/s** | 1.08 M msg/s | **5.1x faster** |
| 1 KB | 314 K msg/s | **2.3 M msg/s** | 667 K msg/s | **3.5x faster** |
| 4 KB | 282 K msg/s | **857 K msg/s** | 314 K msg/s | **2.7x faster** |
| 16 KB | 252 K msg/s | 265 K msg/s | 111 K msg/s | **2.4x faster** |

The eager mode's lower numbers vs zmq come from one io_uring SQ entry per
message. Write coalescing batches ~970 x 64 B messages (or ~240 x 256 B) into
one `write_all()` call, eliminating the per-message kernel boundary crossing.
libzmq achieves its batching via an internal IO thread; monocoque's coalescing
is explicit but achieves a higher batch ratio with zero intermediate copies.

For **large** frames, eager mode no longer copies the body into the send buffer:
above `vectored_write_threshold` (default 32 KB) it writes the header and the
`Bytes` body as an iovec (`writev`), so the payload is never copied on its way to
the kernel. See [Vectored writes](#vectored-writes-large-frames) below.

---

### Throughput - DEALER/ROUTER batch API, 10 000 messages, batches of 100

The explicit `send_buffered() / flush()` API (used by the pipelined benchmark)
encodes N messages then issues one write, similar to coalescing but with manual
control over batch boundaries:

| Message size | Throughput | Bandwidth |
|---|---|---|
| 64 B | 2.45 M msg/s | 150 MiB/s |
| 256 B | 1.98 M msg/s | 484 MiB/s |
| 1 KB | 1.08 M msg/s | 1.03 GiB/s |
| 4 KB | 387 K msg/s | 1.48 GiB/s |

---

### Latency - REQ/REP round-trip

Each iteration: 1 000 warmup rounds on a fresh connection (not measured), then
one send + recv + socket teardown. Because teardown is included (TCP FIN + thread
join), these numbers are higher than steady-state RTT on a persistent connection.
Both monocoque and zmq are measured identically.

| Message size | monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64 B | 63 µs | 304 µs | 79% lower |
| 256 B | 72 µs | 296 µs | 76% lower |
| 1 KB | 72 µs | 312 µs | 77% lower |

The monocoque advantage here is mainly that `drop(socket)` is faster: the
async socket cleanup completes synchronously within `block_on`, while zmq's
socket destructor involves thread synchronization.

---

### IPC vs TCP loopback

Unix domain sockets (`UnixStream`) skip the IP stack entirely; on Linux they
share kernel buffers without copying.

**Latency (REQ/REP, including teardown):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 77 µs | 82 µs | 78 µs |
| IPC | 83 µs | 84 µs | 92 µs |
| IPC vs TCP | comparable | comparable | comparable |

On this run IPC and TCP latency land within each other's noise band. The
per-iteration teardown (FIN/close plus thread join) dominates the measurement
and is similar for both transports, so IPC's lower per-message cost does not
show up on the latency axis. The IPC advantage is on throughput, below.

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 340 K msg/s | 341 K msg/s | 309 K msg/s |
| IPC | 716 K msg/s | 702 K msg/s | 662 K msg/s |
| IPC speedup | 2.1x faster | 2.1x faster | 2.1x faster |

IPC throughput is ~2.1x TCP loopback for small messages because Unix sockets
have lower per-syscall overhead and no TCP framing cost.

---

### PUB/SUB patterns

Both run sender and subscribers on separate OS threads against the same peer
under test (monocoque vs rust-zmq).

**Fan-out** (single subscriber, 64 B messages):

| | Latency per message | Throughput |
|---|---|---|
| monocoque | 37 µs | 2.72 M msg/s |
| rust-zmq | 115 µs | 870 K msg/s |
| monocoque vs zmq | | **3.1x faster** |

**Topic filtering** (10% of messages match the subscription):

| | Latency per message | Throughput |
|---|---|---|
| monocoque | 5.7 µs | 17.4 M msg/s |
| rust-zmq | 6.2 µs | 16.1 M msg/s |
| monocoque vs zmq | | **1.08x faster** |

After a profiling pass on the PUB data path, monocoque leads on both: a large
margin on fan-out and a slight edge on topic filtering. The topic-filtering
result is close enough that it moves run to run; the fan-out lead is consistent.

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
copy wins. On this 4-core test box vectored writes are ~1.1–1.3x for 32 KB–1 MB
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

On the coalesced PUSH/PULL throughput bench this lifts 64 B from 7.7 M to 9.7 M
msg/s (about 1.25x) and 256 B by ~15%; the gain tapers as messages grow and the
path becomes bandwidth-bound. `recv()` and `try_recv()` are unchanged for callers
that want an owned `Vec`. A runnable example lives at
`examples/recv_into_zero_alloc.rs`.

---

## Worker pools (fan-out / fan-in)

A single `PushSocket` or `PullSocket` owns one connection. For pool topologies,
`PushFanOut` binds once and round-robins each `send` across N PULL workers, and
`PullFanIn` binds once and merges N PUSH workers into one fair-queued stream.

Two notes from measuring these:

- The `PullFanIn` sink runs all its per-connection readers on **one** runtime and
  batches each kernel-read burst across the merge channel as a single item. That
  keeps it at the single-core decode ceiling (it matches single-stream PULL).
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

Avoid sharing sockets across threads; monocoque sockets are `!Send`.

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
