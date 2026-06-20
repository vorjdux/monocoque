# Performance

## Benchmark results

Measured on loopback TCP against rust-zmq (FFI bindings to libzmq):

**Latency (REQ/REP round-trip)**

| Message size | Monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64B | 23μs | 34μs | 31% faster |
| 256B | 22μs | 35μs | 36% faster |
| 1KB | 23μs | 36μs | 35% faster |

**Throughput (DEALER/ROUTER, batching API, 10k messages)**

| Message size | Throughput |
|---|---|
| 64B | 3.24M msg/s |
| 256B | 2.49M msg/s |
| 1KB | 1.08M msg/s |

IPC (Unix domain sockets) runs 7-17% faster than TCP loopback for local communication.

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

## Profiling checklist

1. **Allocation hot spots**: Run under `dhat` or `heaptrack` and look for
   `Bytes::copy_from_slice` in encode paths.
2. **Lock contention**: Use `tokio-console` or `perf lock` on the routing/pubsub
   hubs under many-peer workloads.
3. **CPU affinity**: Pin runtime threads to specific cores with `nix::sched::sched_setaffinity`.
4. **Kernel version**: Verify io_uring is active (`/proc/$(pidof yourapp)/fdinfo`  -  look for `uring`).
