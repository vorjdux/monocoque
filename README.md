<div align="center">

<img src="assets/monocoque-logo.png" alt="Monocoque Logo" width="600"/>

# Monocoque

> _A Rust-native ZeroMQ-compatible messaging runtime built on `io_uring`_

[![CI](https://github.com/vorjdux/monocoque/actions/workflows/ci.yml/badge.svg)](https://github.com/vorjdux/monocoque/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/monocoque-rs.svg)](https://crates.io/crates/monocoque-rs)
[![docs.rs](https://docs.rs/monocoque-rs/badge.svg)](https://docs.rs/monocoque-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

---

Monocoque is a ZeroMQ-compatible messaging library written in Rust. It implements ZMTP 3.1 from scratch on top of `io_uring` (via `compio`), so it interoperates with any existing libzmq peer while staying entirely within Rust's memory model.

The name comes from Formula 1 engineering, where the monocoque chassis achieves structural strength through form rather than bolt-on reinforcement. Same idea here: performance through correct architecture, not unsafe shortcuts.

## Features

- All 11 ZeroMQ socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- PLAIN and CURVE (CurveZMQ/X25519) authentication, ZAP support
- TCP and IPC (Unix domain socket) transports
- Automatic reconnection with exponential backoff on all socket types
- ZMTP 3.1 heartbeating (PING/PONG) wired into all send/recv loops
- Socket monitoring via channel-based lifecycle events
- Explicit batching API for maximum throughput
- Zero-copy message passing via `Bytes` refcounting

## Performance

Benchmarked against rust-zmq (FFI bindings to libzmq). Separate OS threads for
sender and receiver, real loopback TCP, Linux 6.18, release build.

**PUSH/PULL throughput with write coalescing** (`with_write_coalescing(true)`):

| Message size | monocoque | rust-zmq | Ratio |
|---|---|---|---|
| 64 B | **6.1 M msg/s** | 971 K msg/s | 6.3× faster |
| 256 B | **3.5 M msg/s** | 699 K msg/s | 5.0× faster |
| 1 KB | **1.4 M msg/s** | 455 K msg/s | 3.1× faster |
| 4 KB | **391 K msg/s** | 168 K msg/s | 2.3× faster |
| 16 KB | **113 K msg/s** | 71 K msg/s | 1.6× faster |

Default (eager) mode sends each message immediately and is suitable when latency
matters more than throughput. IPC (Unix domain sockets) is ~2.4× faster than
TCP loopback for same-host communication. See [docs/performance.md](docs/performance.md)
for the full breakdown including latency numbers and tuning guidance.

## Quick Start

```toml
[dependencies]
monocoque-rs = { version = "0.1", features = ["zmq"] }
compio = { version = "0.13", features = ["runtime"] }
```

```rust
use monocoque::zmq::{DealerSocket, RouterSocket};

// Connect a DEALER
let mut dealer = DealerSocket::connect("tcp://127.0.0.1:5555").await?;
dealer.send(vec![b"Hello".into()]).await?;
let reply = dealer.recv().await?;

// Bind a ROUTER
let mut router = RouterSocket::bind("tcp://127.0.0.1:5555").await?;
let msg = router.recv().await?;  // msg[0] is the routing identity
```

```rust
// PUB/SUB
let mut publisher = PubSocket::bind("tcp://127.0.0.1:5556").await?;
publisher.send(vec![b"events".into(), b"payload".into()]).await?;

let mut subscriber = SubSocket::connect("tcp://127.0.0.1:5556").await?;
subscriber.subscribe(b"events").await?;
let msg = subscriber.recv().await?;
```

For high throughput, enable write coalescing or use the explicit batch API.

By default each `send()` issues one kernel write per message. Write coalescing batches
those writes into a 64 KB buffer and flushes them in a single syscall, which is where
the large throughput gains in the table above come from. Because messages may sit in
userspace until `flush()` is called, coalescing is opt-in: you decide exactly when the
data goes out. See [docs/performance.md](docs/performance.md) for the full explanation
and tuning guide.

```rust
// Write coalescing: opt-in, requires flush() after each burst (PUSH/PULL)
let mut push = PushSocket::connect_with_options(
    "127.0.0.1:5555",
    SocketOptions::default().with_write_coalescing(true),
).await?;
for msg in &batch {
    push.send(vec![msg.clone()]).await?;
}
push.flush().await?;  // flush bytes that did not fill the 64 KB threshold

// Explicit batch API: encode N messages then one write (DEALER/ROUTER)
for msg in &batch {
    dealer.send_buffered(msg.clone())?;
}
dealer.flush().await?;
```

## Safety

`unsafe` code is confined to a single file: `monocoque-core/src/alloc.rs`, which implements the arena allocator for io_uring-safe buffer management. Everything else is 100% safe Rust.

Memory invariants:
- Buffers are never reused while referenced (tracked via `Bytes` refcounts)
- `SlabMut` -> `Bytes` is a one-way transition; no mutation after freeze
- PUB fanout is refcount-based (`Bytes::clone()`), never copies payloads

## Development

```bash
cargo build --release --workspace
cargo test --workspace --features zmq
cargo bench --features zmq       # runs the benchmark suite
```

Interop testing against libzmq: see [docs/INTEROP_TESTING.md](docs/INTEROP_TESTING.md).

## Roadmap

Core features are complete. Possible future work:

- io_uring fixed buffers (`IORING_OP_READ_FIXED`) - removes the last kernel-boundary copy per read; ~5-15% latency improvement at an already low baseline
- Prefix trie for topic matching - only relevant with 100+ concurrent subscribers using deep topic hierarchies
- Concurrent PUB fanout - prevents one slow subscriber from delaying others in large-subscriber deployments

Long term: high-performance RPC, additional transports (QUIC, shared memory), custom protocol framework.

## License

MIT - see [LICENSE](LICENSE).

---

Built with: `compio`, `bytes`, `flume`, `smallvec`
