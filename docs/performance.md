# Performance

## Benchmark results

Measured on loopback TCP against rust-zmq (FFI bindings to libzmq).
Hardware: Intel Core i7-1355U (12 threads), Linux 6.17, rustc 1.91, release build.
Latency is the steady-state round-trip over a persistent connection (connection
setup excluded), reported as the median of 100 samples.

**Latency (REQ/REP round-trip)**

| Message size | Monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64B | 7.3μs | 25.9μs | 72% faster |
| 256B | 7.3μs | 27.8μs | 74% faster |
| 1KB | 7.5μs | 25.6μs | 71% faster |

**Throughput (DEALER/ROUTER, batching API, 10k messages)**

| Message size | Throughput |
|---|---|
| 64B | 2.97M msg/s |
| 256B | 2.52M msg/s |
| 1KB | 1.23M msg/s |

Synchronous (non-pipelined) DEALER/ROUTER throughput is about 120k to 133k msg/s
for monocoque versus about 34k msg/s for rust-zmq, roughly 4x faster.

IPC (Unix domain sockets) runs about 35% faster than TCP loopback for local
communication. Latency is 4.8 to 5.1μs versus 7.4 to 8.0μs, and throughput is
26% to 39% higher.

Run the benchmarks: `cargo bench --features zmq` from the `monocoque/` directory.

---

## Tuning

monocoque is built on [compio](https://github.com/compio-rs/compio), which uses
`io_uring` on Linux for minimal syscall overhead and zero-copy I/O. This guide
covers the main knobs for squeezing out throughput and latency.

---

## Buffer sizes

The two most impactful options are the read and write buffer sizes. The default
is 8 KB, which balances latency and throughput for mixed workloads.

```rust
use monocoque_core::options::SocketOptions;

// Low-latency REQ/REP with small messages (< 1 KB)
let opts = SocketOptions::small(); // 4 KB buffers

// High-throughput PUSH/PULL with large messages (> 8 KB)
let opts = SocketOptions::large(); // 16 KB buffers

// Custom
let opts = SocketOptions::default()
    .with_buffer_sizes(32_768, 32_768); // 32 KB
```

**Rule of thumb:** set read buffer ≥ your largest expected message frame.
Oversized buffers waste memory; undersized buffers trigger extra syscalls.

---

## High-water marks (HWM)

HWM limits the number of messages queued in memory. When the queue is full,
sends block (or drop on PUB sockets). Lower HWM = lower memory footprint
and faster backpressure propagation; higher HWM = more buffering to absorb bursts.

```rust
let opts = SocketOptions::default()
    .with_send_hwm(500)   // halve the default send queue
    .with_recv_hwm(2000); // double the default recv queue
```

Setting HWM to 0 disables the limit entirely (unbounded queue  -  use with care).

---

## io_uring tuning

compio uses a shared io_uring ring per thread. On Linux ≥ 5.11 you get
the full benefit; older kernels fall back to thread-pool I/O.

- **SQ/CQ ring size**: Controlled by compio's runtime builder. Larger rings
  reduce submission overhead for high-connection-count servers.
- **SQPOLL**: Enables kernel-side submission polling, eliminating `io_uring_enter`
  syscalls at the cost of a dedicated CPU core. Useful only for
  sustained > 500k msg/s workloads.
- **Fixed buffers**: compio's arena allocator already pins buffers for io_uring
  registered buffers. No extra work needed.

---

## Thread model

Each compio runtime is **single-threaded**. For multi-core throughput, run one
runtime per core and connect sockets across runtimes using the `inproc` transport
(zero-copy, no serialisation).

```rust
// Spawn N worker threads, each with their own runtime
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
|-----------|----------------|-----------|
| inproc    | < 1 µs         | ✅ Arc clone |
| TCP loopback | 10–50 µs    | ✅ io_uring |
| TCP LAN   | 100–500 µs     | ✅ io_uring |

Use inproc for inter-task communication within a process, TCP for cross-process.

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
    .with_heartbeat_ivl(Duration::from_secs(20))     // send PING after 20s idle
    .with_heartbeat_timeout(Duration::from_secs(5));  // expect PONG within 5s
```

---

## TCP keepalive

TCP_NODELAY is on by default. For long-lived connections crossing NAT or firewalls, enable OS-level keepalive:

```rust
let opts = SocketOptions::default()
    .with_tcp_keepalive(1)
    .with_tcp_keepalive_idle(60)    // seconds before first probe
    .with_tcp_keepalive_intvl(10)   // seconds between probes
    .with_tcp_keepalive_cnt(5);     // probes before giving up
```

---

## Checklist

- Build with `--release` - debug builds are 5-10x slower
- Linux kernel >= 5.11 for full io_uring benefit
- Size read/write buffers to match your 99th-percentile message size
- Use IPC instead of TCP loopback for co-located sockets
- Run `dhat` or `heaptrack` to catch `Bytes::copy_from_slice` in hot paths
- Verify io_uring is active: `/proc/$(pidof yourapp)/fdinfo` (look for `uring`)
