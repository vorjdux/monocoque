# Performance Tuning Guide

Reference numbers: 23 μs round-trip latency, 3.24 M msg/sec throughput (benchmarked on a Linux kernel 6.x host with io_uring support).

---

## 1. PubSocket Worker Count

`PubSocket` uses a thread-per-core worker pool. Each worker owns a shard of subscribers and runs its own io_uring ring.

```rust,no_run
use monocoque::zmq::PubSocket;

// Default: one worker per logical CPU (good for ≥4 cores)
let socket = PubSocket::bind("127.0.0.1:5555").await?;

// Override  -  tune this when:
//   • you have many other CPU-heavy tasks on the same host
//   • you have very few subscribers (1 worker is enough)
//   • you are running in a container with a low CPU quota
let socket = PubSocket::bind_with_workers("127.0.0.1:5555", 4).await?;
```

**Rule of thumb**:

| Subscriber count | Recommended workers |
|-----------------|-------------------|
| 1 – 50 | 1 |
| 50 – 500 | `num_cpus / 2` |
| 500+ | `num_cpus` (default) |

Worker threads are idle when there is no traffic, so over-provisioning is safe.

---

## 2. Buffer Size Tuning

The `read_buffer_size` and `write_buffer_size` fields on `SocketOptions` control the size of the io_uring read/write buffers submitted per operation.

```rust,no_run
use monocoque::zmq::{DealerSocket, SocketOptions};

let options = SocketOptions::default()
    .with_read_buffer_size(64 * 1024)   // 64 KB  -  good for large messages
    .with_write_buffer_size(64 * 1024);

let socket = DealerSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

| Workload | `read_buffer_size` | `write_buffer_size` |
|----------|-------------------|-------------------|
| Small messages (<1 KB) | 4–8 KB (default) | 4–8 KB |
| Medium messages (1–64 KB) | 16–64 KB | 16–64 KB |
| Large blobs (>64 KB) | 128–256 KB | 128–256 KB |
| Latency-critical | Keep at 4–8 KB (smaller = fewer wasted bytes per partial read) |

Predefined presets:

```rust,no_run
let options = SocketOptions::small();   // 4 KB buffers
let options = SocketOptions::default(); // 8 KB buffers
let options = SocketOptions::large();   // 16 KB buffers
```

---

## 3. TCP_NODELAY and Keepalive

**TCP_NODELAY** is enabled by default on all sockets. It disables Nagle's algorithm, which would batch small writes and add up to 40 ms of artificial latency. Do not disable it unless you are sending many tiny messages and want the OS to coalesce them (rare).

**TCP keepalive** is useful for long-lived connections crossing NAT or firewalls:

```rust,no_run
use monocoque::zmq::SocketOptions;

let options = SocketOptions::default()
    .with_tcp_keepalive(1)          // enable keepalive (1 = on, 0 = off, -1 = OS default)
    .with_tcp_keepalive_idle(60)    // seconds before first probe after last data
    .with_tcp_keepalive_intvl(10)   // seconds between probes
    .with_tcp_keepalive_cnt(5);     // number of probes before giving up
```

For short-lived connections or same-host communication, keepalive adds no value and can be left at `-1` (OS default, usually disabled).

---

## 4. inproc vs TCP for Same-Process Communication

When both ends of a socket pair live in the same process, use the inproc transport instead of TCP loopback:

```rust,no_run
use monocoque::zmq::ipc; // inproc helpers (Unix only)

// TCP loopback  -  crosses kernel network stack (~5–10 μs overhead)
let dealer = DealerSocket::connect("127.0.0.1:5555").await?;

// IPC (Unix domain socket)  -  stays in kernel, skips TCP framing (~7–17% faster)
let dealer = DealerSocket::connect("ipc:///tmp/myapp.sock").await?;
```

Use IPC when:
- Both peers are on the same host and in the same process or sibling processes
- You need the lowest possible same-host latency

Use TCP when:
- You need to connect across hosts
- You want firewall / TLS / load-balancer compatibility
- You may move peers to separate hosts in the future

---

## 5. High-Water Mark (HWM) and Backpressure

The HWM controls how many messages can queue in the outbound channel before the socket applies backpressure (DEALER/ROUTER/PUB) or drops (PUB workers).

```rust,no_run
use monocoque::zmq::SocketOptions;

let options = SocketOptions::default()
    .with_send_hwm(5_000)   // default 1 000; increase for burst-tolerant workloads
    .with_recv_hwm(5_000);
```

For `PubSocket`, a full worker channel drops the message and increments an internal counter accessible via `socket.drop_count()`. Monitor this in production  -  a non-zero value means subscribers are too slow.

---

## 6. Quick-Reference Checklist

- [ ] `cargo build --release`  -  debug builds are 5-10× slower
- [ ] Linux kernel ≥ 5.11  -  earlier kernels have slower io_uring paths
- [ ] Set `PubSocket` worker count to match subscriber concurrency
- [ ] Size read/write buffers to match your 99th-percentile message size
- [ ] Leave `TCP_NODELAY` enabled (default)
- [ ] Use IPC instead of TCP loopback for co-located sockets
- [ ] Monitor `PubSocket::drop_count()` in production for backpressure signals
