<div align="center">

<img src="assets/monocoque-logo.png" alt="Monocoque Logo" width="600"/>

# Monocoque

> _A Rust-native ZeroMQ-compatible messaging runtime built on `io_uring`_

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

Benchmarked against rust-zmq (FFI bindings to libzmq):

| Message size | Monocoque | rust-zmq | Improvement |
|---|---|---|---|
| 64B | 21μs | 31μs | 32% faster |
| 256B | 22μs | 31μs | 29% faster |
| 1024B | 22μs | 33μs | 31% faster |

Throughput with the batching API: 2M+ msg/sec. IPC is 7-10% faster than TCP loopback for local communication.

## Quick Start

```toml
[dependencies]
monocoque = { version = "0.1", features = ["zmq"] }
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

For high throughput, buffer messages and flush once:

```rust
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
