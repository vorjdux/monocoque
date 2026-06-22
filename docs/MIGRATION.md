# Migration Guide

This covers migrating to monocoque from libzmq (C) or rust-zmq.

---

## Dependency change

Remove `zmq` from your `Cargo.toml` and add:

```toml
[dependencies]
monocoque-rs = "0.1"
bytes = "1.0"
compio = "0.12"
```

You no longer need libzmq installed on the system. Monocoque is pure Rust.

---

## The big differences

**No context.** libzmq and rust-zmq require a `Context` object that owns all sockets. Monocoque sockets are independent - just create them directly.

**Async everywhere.** All socket operations are `async`. You need a compio runtime: add `#[compio::main]` to your entry point. There's no blocking API.

**`recv()` returns `Result<Option<Vec<Bytes>>>`.** This is the change that will touch the most code. In rust-zmq, `recv_msg(0)` returns `Result<Message>` where the error encodes both I/O problems and timeouts (via errno). In monocoque, `Ok(None)` means the connection closed or a timeout elapsed - it's not an error. `Err(e)` is an actual I/O problem. Update all your recv callsites:

```rust
// rust-zmq
let msg = socket.recv_msg(0)?;

// monocoque - handle the None case
match socket.recv().await {
    Ok(Some(msg)) => { /* use msg */ }
    Ok(None) => { /* timeout or peer closed */ }
    Err(e) => { /* I/O error */ }
}
```

If you're confident the socket is live and you just want to unwrap, `socket.recv().await.expect("connection closed")` is the common idiom in examples, but you probably want proper error handling in production.

**Multipart messages are `Vec<Bytes>`.** Instead of calling `send` multiple times with `ZMQ_SNDMORE`, or building up frames with `recv` + `get_more()`, you send and receive the entire multipart message at once. Frame 0 is frame 0 in the vec, etc.

```rust
// rust-zmq multipart send
socket.send("part1", zmq::SNDMORE)?;
socket.send("part2", 0)?;

// monocoque
socket.send(vec![Bytes::from("part1"), Bytes::from("part2")]).await?;
```

**Socket options use a builder.** Instead of individual setter calls, build a `SocketOptions` and pass it at construction:

```rust
// rust-zmq
socket.set_rcvtimeo(5000)?;
socket.set_rcvhwm(1000)?;

// monocoque
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_millis(5000))
    .with_recv_hwm(1000);
let socket = DealerSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

**Transport strings.** Monocoque currently only supports TCP and IPC. The address format for TCP is a bare host:port string, not `tcp://...`:

```rust
// libzmq / rust-zmq
socket.connect("tcp://localhost:5555")?;

// monocoque
DealerSocket::connect("127.0.0.1:5555").await?;
```

**Socket type names.** `ZMQ_DEALER` becomes `DealerSocket`, `ZMQ_ROUTER` becomes `RouterSocket`, and so on. The pattern is consistent for all 11 socket types.

---

## What's not there yet

- **inproc transport** - partially supported via a channel bridge, but not the same as libzmq inproc.
- **`zmq_poll()`** - use `futures::select!` to multiplex across sockets.
- **`ZMQ_STREAM`** - `StreamSocket` exists but check current docs for limitations.

Everything else - DEALER/ROUTER, PUB/SUB, PUSH/PULL, PAIR, XPUB/XSUB, PLAIN/CURVE security, ZAP, socket monitor, proxy - is supported.

---

## Migration checklist

- Remove libzmq system dependency
- Replace `zmq` crate with `monocoque` + `bytes` + `compio`
- Add `#[compio::main]` to entry points
- Remove `Context::new()`, create sockets directly
- Convert all recv callsites to handle `Result<Option<Vec<Bytes>>>`
- Replace `SNDMORE` multi-send with `Vec<Bytes>` single send
- Replace `recv` + `get_more()` loops with single `recv()` returning all frames
- Convert socket option setters to `SocketOptions` builder
- Strip `tcp://` prefix from addresses (use bare `host:port`)
- Replace `zmq_poll()` with `futures::select!`
