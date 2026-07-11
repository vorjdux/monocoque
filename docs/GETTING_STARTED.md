# Getting Started with Monocoque

A five-minute guide to sending your first message with Monocoque.

**Performance Highlights:**

- **~5x lower latency** than libzmq (43-58 µs vs ~270 µs REQ/REP round-trip)
- **Up to 17.1 M msg/sec throughput** with write coalescing
- **Three runtimes** - io_uring via compio (default), or epoll via tokio, or async-io via smol, same API
- **Pure Rust** - no C dependencies, full async/await
- **Memory safe** - unsafe is confined to the owned-buffer read helpers (`core::io`) and the raw-socket tuning facade

---

## Installation

```toml
[dependencies]
monocoque-rs = { version = "0.2", features = ["zmq"] }
bytes    = "1"
compio   = { version = "0.10", features = ["runtime", "macros"] }
```

The examples below use the default backend (io_uring via compio) and its
`#[compio::main]` entry point. See the next section if you want to run on tokio.

---

## Choosing a runtime

Monocoque ships three interchangeable runtime backends, selected by a Cargo
feature. The protocol, codec and API are identical on all of them; only the
runtime primitives differ.

- **`runtime-compio`** (default): native io_uring on Linux. Its edge shows on
  real network I/O and high connection counts.
- **`runtime-tokio`**: standard tokio (epoll/mio). Use it where io_uring is not
  available (macOS, Windows, older kernels) or to fit an existing tokio stack. On
  single-flow loopback microbenchmarks it is actually a touch faster than compio
  (see [performance.md](performance.md)); pick by your real workload, not the
  microbenchmark.
- **`runtime-smol`**: smol (async-executor + async-io). Another portable,
  non-io_uring option, useful where io_uring is unavailable or when you already
  build on the smol stack. It drives sockets on a single-threaded smol
  `LocalExecutor`.

```toml
# tokio backend
[dependencies]
monocoque-rs = { version = "0.2", default-features = false, features = ["runtime-tokio", "zmq"] }
bytes = "1"
tokio = { version = "1", features = ["rt", "macros"] }
```

```toml
# smol backend
[dependencies]
monocoque-rs = { version = "0.2", default-features = false, features = ["runtime-smol", "zmq"] }
bytes = "1"
```

The tokio backend follows the same thread-per-core model as compio, so run it on
a current-thread runtime inside a `LocalSet`:

```rust,no_run
# use monocoque::zmq::ReqSocket;
# use bytes::Bytes;
fn main() -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;
        socket.send(vec![Bytes::from("PING")]).await?;
        let _reply = socket.recv().await;
        Ok::<(), std::io::Error>(())
    })
}
```

To keep your own code free of any runtime name, use `monocoque::rt::LocalRuntime`,
which builds the right single-threaded runtime for whichever feature is enabled.
The `runtime_backends` example is one program that runs unchanged on all three:

```bash
cargo run --example runtime_backends --features zmq                                      # compio
cargo run --example runtime_backends --no-default-features --features runtime-tokio,zmq  # tokio
cargo run --example runtime_backends --no-default-features --features runtime-smol,zmq   # smol
```

---

## Example 1: REQ/REP Round-Trip

The simplest pattern: one client sends a request, one server sends a reply.

**Server** (`src/bin/rep_server.rs`):

```rust,no_run
use compio::net::TcpListener;
use monocoque::zmq::RepSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:5555").await?;
    println!("Server listening on :5555");

    let (stream, _addr) = listener.accept().await?;
    let mut socket = RepSocket::from_tcp(stream).await?;

    if let Some(request) = socket.recv().await {
        println!("Got: {:?}", request);
        socket.send(vec![Bytes::from("PONG")]).await?;
    }
    Ok(())
}
```

**Client** (`src/bin/req_client.rs`):

```rust,no_run
use monocoque::zmq::ReqSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    let mut socket = ReqSocket::connect("127.0.0.1:5555").await?;

    socket.send(vec![Bytes::from("PING")]).await?;

    if let Some(reply) = socket.recv().await {
        println!("Reply: {:?}", reply);  // [b"PONG"]
    }
    Ok(())
}
```

Run the server first, then the client:

```bash
cargo run --bin rep_server
cargo run --bin req_client
```

---

## Example 2: PUB/SUB

A publisher broadcasts events; subscribers filter by topic prefix.

**Publisher** (`src/bin/publisher.rs`):

```rust,no_run
use monocoque::zmq::PubSocket;
use bytes::Bytes;
use std::time::Duration;

#[compio::main]
async fn main() -> std::io::Result<()> {
    let mut socket = PubSocket::bind("127.0.0.1:5556").await?;
    println!("Publisher bound to :5556");

    // Accept one subscriber before publishing.
    socket.accept_subscriber().await?;

    for i in 0u32.. {
        socket.send(vec![
            Bytes::from("weather.london"),
            Bytes::from(format!("temp={}", 15 + i % 10)),
        ]).await?;
        compio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
}
```

**Subscriber** (`src/bin/subscriber.rs`):

```rust,no_run
use monocoque::zmq::SubSocket;

#[compio::main]
async fn main() -> std::io::Result<()> {
    let mut socket = SubSocket::connect("127.0.0.1:5556").await?;
    socket.subscribe(b"weather.london").await?;

    while let Ok(Some(msg)) = socket.recv().await {
        let topic = std::str::from_utf8(&msg[0]).unwrap_or("?");
        let data  = std::str::from_utf8(&msg[1]).unwrap_or("?");
        println!("{}: {}", topic, data);
    }
    Ok(())
}
```

---

## Socket Types at a Glance

| Socket | Pattern | Typical role |
|--------|---------|-------------|
| `ReqSocket` | Request-Reply | Sync client (must alternate send/recv) |
| `RepSocket` | Request-Reply | Sync server (must alternate recv/send) |
| `DealerSocket` | Async Request-Reply | Async client with reconnect support |
| `RouterSocket` | Identity Routing | Server with per-peer routing IDs |
| `PubSocket` | Broadcast | Publisher (worker-pool, many subscribers) |
| `SubSocket` | Filtered recv | Subscriber with topic filters |
| `PushSocket` | Pipeline | Task distributor (one connection) |
| `PullSocket` | Pipeline | Task worker (one connection) |
| `PushFanOut` | Pipeline pool | Ventilator that round-robins across N PULL workers |
| `PullFanIn` | Pipeline pool | Sink that merges results from N PUSH workers |
| `PairSocket` | Exclusive Pair | One-to-one channel |

---

## Running Tests

```bash
# Core protocol tests
cargo test -p monocoque-zmtp

# Full workspace
cargo test --workspace
```

---

## Next Steps

- [Architecture Decision Records](ADR.md)  -  why io_uring, worker pools, and `Bytes`
- [Performance Tuning Guide](PERFORMANCE_TUNING.md)  -  buffer sizes, worker counts, TCP options
- [Security Guide](SECURITY_GUIDE.md)  -  PLAIN and CURVE authentication
- [Migration Guide](MIGRATION.md)  -  coming from `zmq` (rust-zmq) or libzmq
- API docs: `cargo doc --no-deps --open -p monocoque`

---

*License: MIT OR Apache-2.0*
