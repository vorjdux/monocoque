# monocoque-core

**⚠️ Internal Implementation Crate**

This crate is an internal implementation detail of the Monocoque project. It
provides the low-level runtime primitives shared by the protocol crates: the
owned-buffer I/O layer over the io_uring (compio), tokio, and smol backends, the
read slab, socket options, endpoint parsing, byte-based backpressure, and the
inproc transport.

## For Application Development

**Use the `monocoque` crate instead:**

```toml
[dependencies]
monocoque-rs = { version = "0.4", features = ["zmq"] }
```

```rust
use monocoque::zmq::DealerSocket;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
    socket.send(vec![b"Hello!".into()]).await?;
    let response = socket.recv().await;
    Ok(())
}
```

## Architecture

The Monocoque project is organized as:

-   **`monocoque`** - Public API crate (use this)
-   **`monocoque-zmtp`** - Internal ZMTP 3.1 protocol implementation (internal)
-   **`monocoque-core`** - Internal runtime primitives (this crate, internal)
    -   Owned-buffer read/write over compio / tokio / smol
    -   Zero-copy read slab and buffers
    -   Socket options, endpoints, backpressure, inproc transport

## Documentation

See the main project documentation:
[github.com/vorjdux/monocoque](https://github.com/vorjdux/monocoque)
