# Getting Started with Monocoque

A five-minute guide to sending your first message with Monocoque.

**Performance Highlights:**

- **31-37% faster latency** than libzmq (23 μs vs 33-36 μs round-trip)
- **3.24 M msg/sec throughput** with the batching API
- **Pure Rust**  -  no C dependencies, full async/await
- **Memory safe**  -  zero unsafe code in the protocol layer

---

## Installation

```toml
[dependencies]
monocoque-rs = { version = "0.1", features = ["zmq"] }
bytes    = "1"
compio   = { version = "0.10", features = ["runtime", "macros"] }
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
| `PushSocket` | Pipeline | Task distributor |
| `PullSocket` | Pipeline | Task worker |
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
