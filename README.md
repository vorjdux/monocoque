<div align="center">

<img src="assets/monocoque-logo.png" alt="Monocoque Logo" width="600"/>

# Monocoque

> _A Rust-native ZeroMQ-compatible messaging runtime, io_uring by default with optional tokio and smol backends_

[![CI](https://github.com/vorjdux/monocoque/actions/workflows/ci.yml/badge.svg)](https://github.com/vorjdux/monocoque/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/monocoque-rs.svg)](https://crates.io/crates/monocoque-rs)
[![docs.rs](https://docs.rs/monocoque-rs/badge.svg)](https://docs.rs/monocoque-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

---

Monocoque is a ZeroMQ-compatible messaging library written in Rust. It implements ZMTP 3.1 from scratch over a small runtime facade: io_uring by default (via `compio`), with optional tokio and smol backends for portability. Whichever you pick, it interoperates with any existing libzmq peer while staying entirely within Rust's memory model.

The name comes from Formula 1 engineering, where the monocoque chassis achieves structural strength through form rather than bolt-on reinforcement. Same idea here: performance through correct architecture, not unsafe shortcuts.

## Features

- All 11 ZeroMQ socket types: REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB, PUSH, PULL, PAIR
- PLAIN and CURVE (CurveZMQ/X25519) authentication, ZAP support
- TCP and IPC (Unix domain socket) transports
- Automatic reconnection with exponential backoff on all socket types
- ZMTP 3.1 heartbeating (PING/PONG) wired into all send/recv loops
- Socket monitoring via channel-based lifecycle events
- Explicit batching API for maximum throughput, plus `recv_batch()` to drain a
  burst of messages in one `.await`
- Allocation-free receive via `recv_into` / `try_recv_into`: reuse one buffer
  across a hot recv loop instead of allocating a `Vec` per message
- Vectored (`writev`) sends for large frames: the body skips the userspace copy
- PUB fan-out coalesces queued broadcasts into one vectored write per subscriber
- PUSH/PULL worker pools via `PushFanOut` (round-robin ventilator) and
  `PullFanIn` (fair-queued sink)
- Zero-copy message passing via `Bytes` refcounting

## Performance

Benchmarked against rust-zmq (FFI bindings to libzmq). Separate OS threads for
sender and receiver, real loopback TCP, Intel Core i7-1355U (12 threads),
Linux 6.17, release build. The three runtime backends run the identical suite.
compio and tokio are the established throughput figures; the rust-zmq column was
re-measured with a corrected live-connection timer, and smol was added on the same
scale.

**PUSH/PULL throughput with write coalescing** (`with_write_coalescing(true)`):

| Message size | compio | tokio | smol | rust-zmq |
|---|---|---|---|---|
| 64 B | 9.2 M msg/s | **13.6 M msg/s** | 10.1 M msg/s | 4.73 M msg/s |
| 256 B | 5.6 M msg/s | **9.8 M msg/s** | 6.9 M msg/s | 2.66 M msg/s |
| 1 KB | 2.4 M msg/s | **5.3 M msg/s** | 3.0 M msg/s | 1.04 M msg/s |
| 4 KB | 841 K msg/s | **1.74 M msg/s** | 1.05 M msg/s | 394 K msg/s |
| 16 KB | 268 K msg/s | **473 K msg/s** | 342 K msg/s | 120 K msg/s |

All three backends beat libzmq once coalescing batches the writes: ~1.9x (compio),
~2.9x (tokio), ~2.1x (smol) at 64 B, and ~2-4x across the size range. On these
single-flow loopback microbenchmarks the epoll backends (tokio, smol) are the
faster: a one-connection ping-pong does not exercise io_uring's strengths (batched
submission, registered buffers, many concurrent connections) and just pays its
per-op submission overhead. compio (io_uring) is the default and is where the wins
land for real network I/O and high connection counts. Measure on your own workload.

Default (eager) mode sends each message immediately, one syscall per `send()`, and
is the mode for latency-sensitive work where you want each message on the wire now
rather than batched. On a bulk one-way firehose libzmq's internal batching leads
at small sizes; steady-state REQ/REP latency, though, is ~2.7-3.5x lower on every
monocoque backend (~10 µs vs libzmq's ~35 µs). Turn on coalescing for
small-message throughput. For **large** frames eager mode automatically uses a vectored write
(`writev`) so the body is never copied into the send buffer; the threshold
(`vectored_write_threshold`, default 32 KB) is tunable per workload. IPC (Unix
domain sockets) is ~2.1x (compio) to ~3x (tokio) faster than TCP loopback for
same-host throughput.

**PUB/SUB leads libzmq on both axes**: single-subscriber fan-out runs ~2.9x (compio),
~3.5x (tokio), ~3.0x (smol) faster, and topic filtering at 10% match is a near tie. See
[docs/performance.md](docs/performance.md) for the full breakdown including
latency numbers, per-backend tables, the vectored-write crossover measurements,
PUB/SUB pattern results, and tuning guidance.

## Quick Start

```toml
[dependencies]
monocoque-rs = { version = "0.2", features = ["zmq"] }
# Drives the default io_uring backend and provides the #[compio::main] macro.
# To run on tokio or smol instead, see "Runtime backends" below.
compio = { version = "0.10", features = ["runtime", "macros"] }
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

## Runtime backends

Monocoque runs on `io_uring` through compio by default, but the socket stack is
written against a small runtime facade, so it can drive the same code on tokio
or smol instead. Pick one backend at compile time:

```toml
# Default: native io_uring via compio
monocoque-rs = { version = "0.2", features = ["zmq"] }

# Or run on tokio
monocoque-rs = { version = "0.2", default-features = false, features = ["runtime-tokio", "zmq"] }

# Or run on smol
monocoque-rs = { version = "0.2", default-features = false, features = ["runtime-smol", "zmq"] }
```

The three backends are mutually exclusive. The protocol layer, frame codec and
buffer model are identical across all of them: only the connect/spawn/timer
primitives differ. The tokio and smol backends follow compio's thread-per-core
model, so run tokio on a current-thread runtime inside a `LocalSet` (smol uses a
single-threaded `LocalExecutor`; the backend-agnostic `LocalRuntime` below sets
up the right one for you).

```rust
let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
let local = tokio::task::LocalSet::new();
local.block_on(&rt, async {
    let mut push = PushSocket::connect("127.0.0.1:5555").await?;
    push.send(vec![b"hello".into()]).await?;
    Ok::<_, std::io::Error>(())
})?;
```

If you would rather not name a runtime in your own code, `monocoque::rt::LocalRuntime`
is a backend-agnostic entry point: it builds the right single-threaded runtime for
whichever feature is enabled, so the same source runs on either.

```rust
let rt = monocoque::rt::LocalRuntime::new()?;
rt.block_on(async {
    let mut push = PushSocket::connect("127.0.0.1:5555").await?;
    push.send(vec![b"hello".into()]).await?;
    Ok::<_, std::io::Error>(())
})?;
```

The `runtime_backends` example is the same program run both ways:

```bash
cargo run --example runtime_backends --features zmq                                   # compio
cargo run --example runtime_backends --no-default-features --features runtime-tokio,zmq  # tokio
cargo run --example runtime_backends --no-default-features --features runtime-smol,zmq   # smol
```

## Safety

`unsafe` is confined to a handful of small, well-contained spots, each behind a documented contract:

- `monocoque-core/src/io.rs` - the owned-buffer read helpers shared by every backend. `fill_read` owns the workspace's single `set_buf_init` call (declaring how many bytes a read initialized in a buffer's spare capacity), and `take_read_buffer` hands out read-sized slabs from a reused `BytesMut`. The socket read paths call `take_read_buffer` in documented `unsafe` blocks.
- `monocoque-core/src/tcp.rs` (and a few socket-tuning call sites) - TCP socket tuning (nodelay, keepalive) through the raw socket handle.
- `monocoque-zmtp/src/inproc_stream.rs` - the in-process stream adapter that fills an owned buffer.

Everything else is safe Rust.

Memory invariants:
- Buffers are never reused while referenced (tracked via `Bytes` refcounts)
- A read slab is frozen to `Bytes` in a one-way transition; no mutation after freeze
- The read slab is allocated lazily on the first read, so an idle socket holds none
- PUB fanout is refcount-based (`Bytes::clone()`), never copies payloads

## Development

```bash
cargo build --release --workspace
cargo test --workspace --features zmq
cargo bench --features zmq       # runs the benchmark suite

# The same tests and benchmarks also run on the tokio and smol backends
cargo test --workspace --no-default-features --features runtime-tokio,zmq
cargo bench --no-default-features --features runtime-tokio,zmq
cargo test --workspace --no-default-features --features runtime-smol,zmq
cargo bench --no-default-features --features runtime-smol,zmq
```

Interop testing against libzmq: see [docs/INTEROP_TESTING.md](docs/INTEROP_TESTING.md).

## Roadmap

Core features are complete. Possible future work:

- io_uring fixed buffers (`IORING_OP_READ_FIXED`) - removes the last kernel-boundary copy per read; ~5-15% latency improvement at an already low baseline. (Large *writes* already use vectored `writev`.)
- Prefix trie for topic matching - the publisher-side prefilter and per-subscriber matching use a linear prefix scan, which is fast for the handful of distinct prefixes a PUB typically holds; a trie would only help when a single PUB accumulates 100+ *distinct* subscription prefixes or deep hierarchies
- Per-subscriber concurrent writes - PUB fan-out throughput now exceeds libzmq and is sharded across worker threads (each write has a fault-isolation timeout), but writes *within* a worker are sequential, so one slow subscriber can still delay the others on its worker

Long term: high-performance RPC, additional transports (QUIC, shared memory), custom protocol framework.

## License

MIT - see [LICENSE](LICENSE).

---

Built with: `compio` (default backend), `tokio` or `smol` (optional backends), `bytes`, `flume`, `smallvec`
