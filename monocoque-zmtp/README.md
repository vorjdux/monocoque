# monocoque-zmtp

**⚠️ Internal Implementation Crate**

This crate is an internal implementation detail of the Monocoque project. It provides the low-level ZMTP 3.1 protocol implementation with direct stream I/O.

## For Application Development

**Use the `monocoque` crate instead:**

```toml
[dependencies]
monocoque = { version = "0.1", features = ["zmq"] }
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

    -   High-level socket constructors (`connect()`, `bind()`)
    -   Ergonomic error handling
    -   Feature-gated protocols (`zmq`, future: `http`, `redis`, etc.)

-   **`monocoque-zmtp`** - Internal protocol implementation (internal)

    -   Low-level ZMTP 3.1 protocol logic
    -   Direct stream I/O operations
    -   Not intended for direct consumption

-   **`monocoque-core`** - Internal runtime primitives (internal)
    -   Zero-copy allocators and buffers
    -   Generic networking primitives
    -   io_uring integration layer

## Examples

All user-facing examples are in the `monocoque/examples/` directory. This crate contains only internal performance benchmarks in `benches/`.

## Documentation

See the main project documentation: [github.com/vorjdux/monocoque](https://github.com/vorjdux/monocoque)
