# Getting Started with Monocoque

A quick-start guide to using Monocoque for high-performance messaging.

---

## Installation

Add Monocoque to your `Cargo.toml`:

```toml
[dependencies]
monocoque-core = { path = "path/to/monocoque/monocoque-core" }
monocoque-zmtp = { path = "path/to/monocoque/monocoque-zmtp", features = ["runtime"] }
bytes = "1.5"
compio = { version = "0.10", features = ["runtime", "macros"] }
```

---

## Quick Example: DEALER Socket

```rust
use monocoque_zmtp::DealerSocket;
use compio::net::TcpStream;
use bytes::Bytes;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to a ROUTER server
    let stream = TcpStream::connect("127.0.0.1:5555").await?;
    let socket = DealerSocket::new(stream);
    
    // Send a request
    socket.send(vec![Bytes::from("Hello, Server!")]).await?;
    
    // Receive response
    let response = socket.recv().await?;
    println!("Received: {} frames", response.len());
    
    Ok(())
}
```

Or use the prelude for convenience:

```rust
use monocoque_zmtp::prelude::*;
use compio::net::TcpStream;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stream = TcpStream::connect("127.0.0.1:5555").await?;
    let socket = DealerSocket::new(stream);
    // ... rest of code
    Ok(())
}
```

---

## Socket Types

### DEALER

**Use for**: Client-side request-reply with load balancing

```rust
use monocoque_zmtp::DealerSocket;

let socket = DealerSocket::new(stream);
socket.send(vec![Bytes::from("request")]).await?;
let response = socket.recv().await?;
```

### ROUTER

**Use for**: Server-side routing and identity-based messaging

```rust
use monocoque_zmtp::RouterSocket;

let socket = RouterSocket::new(stream);
let message = socket.recv().await?;  // First frame is routing ID
socket.send(message).await?;          // Route back to sender
```

### PUB (Publisher)

**Use for**: Broadcasting events to multiple subscribers

```rust
use monocoque_zmtp::PubSocket;

let socket = PubSocket::new(stream);
socket.send(vec![
    Bytes::from("topic.name"),
    Bytes::from("event data"),
]).await?;
```

### SUB (Subscriber)

**Use for**: Receiving filtered events from publishers

```rust
use monocoque_zmtp::SubSocket;

let socket = SubSocket::new(stream);
socket.subscribe(Bytes::from("topic.")).await?;
let event = socket.recv().await?;
```

---

## Architecture Overview

Monocoque uses a layered architecture:

```
┌─────────────────────────────────┐
│   Application (Your Code)       │  ← High-level socket APIs
├─────────────────────────────────┤
│   monocoque-zmtp                │  ← ZMTP protocol layer
│   - DealerSocket, RouterSocket  │
│   - ZmtpIntegratedActor         │
│   - Session state machine       │
├─────────────────────────────────┤
│   monocoque-core                │  ← Protocol-agnostic core
│   - SocketActor (split pumps)  │
│   - IoArena (slab allocator)   │
│   - Router/PubSub hubs          │
├─────────────────────────────────┤
│   compio (io_uring runtime)     │  ← Async I/O
└─────────────────────────────────┘
```

**Key Design Principles**:
- **Unsafe code** is confined to `monocoque-core/src/alloc.rs`
- **Protocol-agnostic core** enables custom protocols
- **Zero-copy** message handling with `Bytes`
- **Runtime-agnostic** async primitives

---

## Running Tests

### Unit Tests

```bash
cargo test --all-features
```

### Integration Tests with libzmq

First, install libzmq:

```bash
# Ubuntu/Debian
sudo apt install libzmq3-dev

# macOS
brew install zeromq

# Arch Linux
sudo pacman -S zeromq
```

Then run interop tests:

```bash
cargo test --package monocoque-zmtp --test interop_pair --features runtime
cargo test --package monocoque-zmtp --test interop_router --features runtime
cargo test --package monocoque-zmtp --test interop_pubsub --features runtime
```

---

## Examples

See the `examples/` directory for complete, runnable examples:

- `hello_dealer.rs` - Basic DEALER socket usage
- `router_worker_pool.rs` - ROUTER load balancing pattern
- `pubsub_events.rs` - PUB/SUB event distribution

Run an example:

```bash
cargo run --example hello_dealer --features runtime
```

---

## Performance Tips

1. **Use multipart messages**: Reduces syscalls for complex data
2. **Batch sends when possible**: Vectored I/O is more efficient
3. **Enable release mode**: `cargo build --release`
4. **Profile with perf**: Monocoque is designed for profiling

---

## Troubleshooting

### "Connection refused"

Make sure the server is listening on the correct port before connecting.

### "Handshake timeout"

Check that both peers are using compatible ZMTP versions (3.1).

### "Test compilation errors"

Ensure the `runtime` feature is enabled:

```bash
cargo test --features runtime
```

---

## Next Steps

- Read the [blueprints](docs/blueprints/) for architecture details
- Check [IMPLEMENTATION_STATUS.md](docs/IMPLEMENTATION_STATUS.md) for current status
- Review [NEXT_STEPS_ANALYSIS.md](docs/NEXT_STEPS_ANALYSIS.md) for roadmap

---

## Support

- **Issues**: File on GitHub repository
- **Documentation**: Run `cargo doc --open --all-features`
- **Examples**: See `examples/` directory

---

## License

MIT
