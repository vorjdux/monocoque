# monocoque-zmtp

**⚠️ Internal Implementation Crate**

This crate is an internal implementation detail of the Monocoque project. It provides the ZMTP protocol (ZMTP 3.1 and 3.0, plus ZMTP 1.0 and 2.0 framing for legacy peers), its security mechanisms, and the ZeroMQ socket types built on the socket multiplexer in `monocoque-core`.

## For Application Development

**Use the `monocoque` crate instead:**

```toml
[dependencies]
monocoque-rs = { version = "0.4", features = ["zmq"] }
```

```rust
use monocoque::rt::LocalRuntime;
use monocoque::zmq::DealerSocket;

fn main() -> monocoque::zmq::Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut socket = DealerSocket::new();
        socket.connect("tcp://127.0.0.1:5555").await?;
        socket.send("hello").await?;
        let reply = socket.recv().await?;
        println!("{:?}", reply.get(0));
        Ok(())
    })
}
```

## Architecture

The Monocoque project is organized as:

-   **`monocoque`** - Public API crate (use this)

    -   The `monocoque::zmq` namespace and runtime facade
    -   Feature-gated protocols (`zmq`, `draft`)

-   **`monocoque-zmtp`** - Internal protocol implementation (internal)

    -   Greeting, handshake, NULL / PLAIN / CURVE security, ZAP
    -   The ZMTP codec the multiplexer drives
    -   Socket types; the draft types sit behind the `draft` feature
    -   Not intended for direct consumption

-   **`monocoque-core`** - Internal kernel (internal)
    -   Socket multiplexer and the `Msg` type
    -   Owned-buffer I/O over compio / tokio / smol

## Examples

All user-facing examples are in the `monocoque/examples/` directory.

## Documentation

See the main project documentation: [github.com/vorjdux/monocoque](https://github.com/vorjdux/monocoque)
