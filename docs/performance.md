# Performance

## Benchmark results

All numbers measured on loopback against rust-zmq (FFI bindings to libzmq).
Hardware: Linux 6.18, release build, `rustc 1.91`.
Each benchmark runs sender and receiver on **separate OS threads** with separate
`compio` runtimes, so the numbers reflect real kernel TCP/IPC round-trips, not
cooperative task switching within a single runtime.

Throughput timer lives on the receiver side (starts before first recv, stops
after last recv). Latency timer wraps one send + recv + socket teardown per
iteration, after 1 000 warmup rounds on a fresh connection.

Run: `cargo bench --features zmq` from the `monocoque/` directory.

---

### Throughput — PUSH/PULL one-way, 10 000 messages

`monocoque (eager)` — default mode, one kernel write per `send()`.
`monocoque (coalesced)` — `with_write_coalescing(true)`, messages accumulate in
a 64 KB buffer flushed in one syscall; call `flush()` after the last send.

| Message size | monocoque eager | monocoque coalesced | rust-zmq | coalesced vs zmq |
|---|---|---|---|---|
| 64 B | 153 K msg/s | **6.3 M msg/s** | 971 K msg/s | **6.5x faster** |
| 256 B | 150 K msg/s | **3.6 M msg/s** | 699 K msg/s | **5.2x faster** |
| 1 KB | 133 K msg/s | **1.5 M msg/s** | 455 K msg/s | **3.3x faster** |
| 4 KB | 126 K msg/s | **466 K msg/s** | 168 K msg/s | **2.8x faster** |
| 16 KB | 111 K msg/s | 120 K msg/s | 71 K msg/s | **1.7x faster** |

The eager mode's lower numbers vs zmq come from one io_uring SQ entry per
message. Write coalescing batches ~970 x 64 B messages (or ~240 x 256 B) into
one `write_all()` call, eliminating the per-message kernel boundary crossing.
libzmq achieves its batching via an internal IO thread; monocoque's coalescing
is explicit but achieves a higher batch ratio with zero intermediate copies.

---

### Throughput — DEALER/ROUTER batch API, 10 000 messages, batches of 100

The explicit `send_buffered() / flush()` API (used by the pipelined benchmark)
encodes N messages then issues one write, similar to coalescing but with manual
control over batch boundaries:

| Message size | Throughput | Bandwidth |
|---|---|---|
| 64 B | 1.24 M msg/s | 76 MiB/s |
| 256 B | 1.04 M msg/s | 254 MiB/s |
| 1 KB | 597 K msg/s | 583 MiB/s |
| 4 KB | 210 K msg/s | 820 MiB/s |

---

### Latency — REQ/REP round-trip

Each iteration: 1 000 warmup rounds on a fresh connection (not measured), then
one send + recv + socket teardown. Because teardown is included (TCP FIN + thread
join), these numbers are higher than steady-state RTT on a persistent connection.
Both monocoque and zmq are measured identically.

| Message size | monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64 B | 214 µs | 379 µs | 44% lower |
| 256 B | 210 µs | 379 µs | 45% lower |
| 1 KB | 214 µs | 408 µs | 47% lower |

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
| TCP loopback | 322 µs | 249 µs | 260 µs |
| IPC | 248 µs | 248 µs | 241 µs |
| IPC speedup | 23% faster | similar | 7% faster |

**Throughput (PUSH/PULL eager, 10 000 messages):**

| Transport | 64 B | 256 B | 1 KB |
|---|---|---|---|
| TCP loopback | 150 K msg/s | 148 K msg/s | 132 K msg/s |
| IPC | 357 K msg/s | 347 K msg/s | 329 K msg/s |
| IPC speedup | 2.4x faster | 2.3x faster | 2.5x faster |

IPC throughput is ~2.4x TCP loopback for small messages because Unix sockets
have lower per-syscall overhead and no TCP framing cost.

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
- Use `with_write_coalescing(true)` + `flush()` for throughput-bound workloads
- Size read/write buffers to match your 99th-percentile message size
- Use IPC instead of TCP loopback for co-located sockets (~2.4x throughput gain)
- Run `dhat` or `heaptrack` to catch `Bytes::copy_from_slice` in hot paths
- Verify io_uring is active: `/proc/$(pidof yourapp)/fdinfo` (look for `uring`)
