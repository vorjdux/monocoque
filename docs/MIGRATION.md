# Migration Guide

Two audiences: users of libzmq or rust-zmq moving to monocoque, and users of
monocoque 0.4 moving to the unified message and multiplexer release. Both are
covered below; the second section lists every source-breaking change.

---

## From libzmq or rust-zmq

### Dependency

Remove `zmq` and add:

```toml
[dependencies]
monocoque-rs = "0.4"
```

No system libzmq is needed. The default backend is compio (io_uring on Linux).
For tokio build with `default-features = false, features = ["runtime-tokio", "zmq"]`,
for smol with `features = ["runtime-smol", "zmq"]`.

### No context, async everywhere

Sockets are created directly; there is no `Context`. Every operation is
`async` and runs on a single-threaded runtime. `monocoque::rt::LocalRuntime`
starts the right one for the selected backend:

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

Endpoints use the libzmq syntax: `tcp://host:port`, `ipc:///path`,
`inproc://name`. `bind` returns the endpoint actually bound, so
`tcp://127.0.0.1:0` reports its assigned port.

### Messages

A multipart message is one `Msg`, sent and received whole. There is no
`SNDMORE` and no `get_more()`.

```rust
// rust-zmq
socket.send("part1", zmq::SNDMORE)?;
socket.send("part2", 0)?;

// monocoque
socket.send(Msg::from([Bytes::from("part1"), Bytes::from("part2")])).await?;
```

`recv()` returns a `MsgRef<'_>` that borrows a slot inside the socket, so a
receive allocates nothing. Use `recv_owned()` when the message must outlive the
next call. Errors are `MonocoqueError`: `Again` when a non-blocking operation
would block, `TimedOut` when `recv_timeout` or `send_timeout` elapses,
`HostUnreachable` for ROUTER/SERVER/STREAM sends to an unknown peer.

### Options

Options are set once, through a builder, at construction:

```rust
let socket = DealerSocket::with_options(
    SocketOptions::default()
        .with_recv_timeout(Duration::from_secs(5))
        .with_recv_hwm(1000),
);
```

### poll, proxy, metadata

`monocoque::zmq::poll::poll` is `zmq_poll` over any mix of socket kinds.
`proxy` and `proxy_steerable` match libzmq's. A received message exposes its
connection's properties with `gets("Socket-Type")`, `gets("User-Id")` and so on,
like `zmq_msg_gets`.

### Checklist

- Replace `zmq` with `monocoque-rs`; drop the system libzmq.
- Run socket code inside `LocalRuntime::block_on` (or your backend's runtime).
- Remove `Context`; create sockets directly.
- Replace `SNDMORE` sequences with one `Msg`.
- Replace setters with the `SocketOptions` builder.
- Call `recv_owned()` wherever a received message is kept.

---

## From monocoque 0.4

This release replaces the per-socket I/O code with one multiplexer and one
message type. It is a single break with no compatibility shims.

### Messages

- `Vec<Bytes>` is gone from every signature; `Msg` replaces it. There is no
  `From<Vec<Bytes>>` bridge. Build messages with `Msg::from(bytes)`,
  `Msg::from([a, b])`, `Msg::from("text")`, or collect an iterator of `Bytes`.
- `message_builder::Message` and the `message` module are removed.
- `recv()` returns `Result<MsgRef<'_>>` instead of `Result<Option<Vec<Bytes>>>`.
  A closed connection or a timeout is an error (`TimedOut`), not `Ok(None)`.
  Code that keeps the message calls `recv_owned()`.
- `recv_into` and `try_recv_into` are removed; the borrowed `recv` supersedes
  them. `send_one` is removed; `send(Msg::from(frame))` no longer allocates.

### Errors

- Every public method returns `Result<_, MonocoqueError>`; nothing returns
  `io::Result`. I/O errors arrive as `MonocoqueError::Io`.

### Sockets

- "bind accepts exactly one peer" is gone: every socket type is multi-peer, on
  bind and on connect, and may bind and connect several endpoints.
- `PushFanOut` and `PullFanIn` are removed; `PushSocket` and `PullSocket` load
  balance and fair-queue natively.
- `connect` returns once the dial is started. Sends wait for a peer (or fail
  with `Again`/`TimedOut` per the options), as in libzmq.

### Options

- The `small()` and `large()` presets are removed.
- `monocoque_core::io::READ_SLAB_SIZE` is `MAX_READ_SIZE` (still 64 KiB, the
  ceiling `read_buffer_size` clamps to), and `io::take_read_buffer` is removed.
- `write_buffer_size`, `with_write_buffer_size` and `with_buffer_sizes` are
  removed; the outbound buffer grows on demand and is reused, so the setting
  had no effect. Use `with_read_buffer_size` for the read side.
- `max_reconnect_attempts`, `xpub_manual`, `xpub_welcome_msg` and
  `connect_routing_id` now take effect; they were accepted and ignored.
- `with_conflate()` now takes effect: a conflating socket keeps only the newest
  message. Code that set it and relied on it being ignored changes behaviour.
- `recv_hwm` applies to every socket type, not only STREAM.
- `hwm == 0` means unbounded, as in libzmq.
- `vectored_write_threshold` defaults to 16 KiB (was 32 KiB).

### inproc

- inproc is bounded by the HWM in each direction instead of unbounded, and it
  works across threads as well as within one.

### Security

- CURVE now speaks RFC 26 and interoperates with libzmq. A 0.4 peer cannot
  complete a CURVE handshake with this release; upgrade both ends together.
- `ZapHandler` and `PlainAuthHandler` use native `async fn`. They can no longer
  be used as `dyn` trait objects; store a concrete type, for example
  `Arc<StaticPlainHandler>`. The `async-trait` dependency is gone.

### Draft sockets

- CLIENT/SERVER, RADIO/DISH, SCATTER/GATHER and CHANNEL exist behind the
  `draft` feature.
