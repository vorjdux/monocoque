# monocoque-core

**⚠️ Internal Implementation Crate**

This crate is an internal implementation detail of the Monocoque project. It
provides the protocol-agnostic kernel the protocol crates build on: the socket
multiplexer (`SocketCore`, one per socket, over any number of peers), the `Msg`
message type, the owned-buffer I/O layer over the io_uring (compio), tokio, and
smol backends, socket options, endpoint parsing, subscription matching, and the
inproc transport.

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
-   **`monocoque-zmtp`** - Internal ZMTP implementation (internal)
-   **`monocoque-core`** - Internal kernel (this crate, internal)
    -   Socket multiplexer: per-peer reader and writer loops, driver task
    -   `Msg` / `MsgRef` and same-thread queues
    -   Owned-buffer read/write over compio / tokio / smol
    -   Socket options, endpoints, subscriptions, inproc transport

## Documentation

See the main project documentation:
[github.com/vorjdux/monocoque](https://github.com/vorjdux/monocoque)
