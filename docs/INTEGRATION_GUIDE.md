# Integration Guide

## Adding monocoque to your project

Add the dependencies to your `Cargo.toml`:

```toml
[dependencies]
monocoque-rs-zmtp = { version = "0.1.0", path = "..." }   # crates.io name
compio = { version = "...", features = ["runtime"] }
bytes = "1"
```

monocoque runs on [compio](https://github.com/compio-rs/compio), a completion-based io_uring runtime, by default. If you are already on tokio, you have two choices: keep the default compio backend and run it on its own thread (the two runtimes do not conflict), or build monocoque with its native tokio backend (`default-features = false, features = ["runtime-tokio", "zmq"]`) and run everything on one tokio runtime. The tokio backend uses a current-thread runtime inside a `LocalSet`.

## Basic patterns

All sockets follow the same construction pattern:

```rust
use monocoque_zmtp::{DealerSocket, RouterSocket, SocketOptions};

// Default options
let socket = DealerSocket::new();

// Custom options
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_hwm(1000);
let socket = DealerSocket::with_options(options);

// Connect directly
let socket = DealerSocket::from_tcp("127.0.0.1:5555").await?;
```

Sending and receiving use `Vec<Bytes>` for multipart messages:

```rust
use bytes::Bytes;

socket.send(vec![Bytes::from("hello")]).await?;

if let Some(frames) = socket.recv().await? {
    // frames is Vec<Bytes>
}
```

For building multipart messages, use the `Message` builder:

```rust
use monocoque_core::Message;

let msg = Message::new()
    .push_str("topic")
    .push_empty()           // envelope delimiter
    .push_str("payload")
    .into_frames();
```

## Transports

Sockets accept `tcp://`, `ipc://`, and `inproc://` endpoint strings:

```rust
socket.connect("tcp://127.0.0.1:5555").await?;
socket.connect("ipc:///tmp/my.sock").await?;   // Unix only
socket.connect("inproc://my-endpoint").await?;
```

## Security

PLAIN (username/password) and CURVE (encrypted) are both available via `SocketOptions`:

```rust
// PLAIN client
let options = SocketOptions::new()
    .with_plain_credentials("user", "password");

// CURVE server
let keypair = CurveKeyPair::generate();
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(*keypair.public.as_bytes(), keypair.secret);
```

See `examples/plain_auth_demo.rs` and `examples/curve_demo.rs` for full working examples.

## Proxies

```rust
use monocoque_zmtp::proxy;

// Forward all messages between frontend and backend
proxy(&mut frontend, &mut backend, None).await?;

// With a control socket (PAUSE/RESUME/TERMINATE)
proxy_steerable(&mut frontend, &mut backend, None, &mut control).await?;
```

## Examples

The `examples/` directory covers the common patterns:

- `req_rep.rs` - basic request/reply
- `pub_sub.rs` - publish/subscribe with topic filtering
- `router_dealer.rs` - async load balancing
- `proxy_broker.rs` - ROUTER-DEALER proxy
- `paranoid_pirate.rs` - reliable request/reply with heartbeating
- `plain_auth_demo.rs` - PLAIN authentication
- `curve_demo.rs` - CURVE encrypted messaging
- `inproc_demo.rs` - in-process messaging

For a broader overview of what's supported, see `docs/ZEROMQ_COMPATIBILITY_ROADMAP.md`. For migrating from an earlier monocoque API, see `docs/MIGRATION.md`.
