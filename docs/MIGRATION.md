# Migration Guide

Two audiences: users of libzmq or rust-zmq moving to monocoque, and users of
monocoque 0.4 moving to 1.0. Both are covered below; the second section lists
every source-breaking change.

---

## From libzmq or rust-zmq

### Dependency

Remove `zmq` and add:

```toml
[dependencies]
monocoque-rs = { version = "1.0", features = ["zmq"] }
```

The sockets live in `monocoque::zmq`, which exists only with the `zmq` feature,
so the feature is required. No system libzmq is needed. The default backend is
compio (io_uring on Linux). The backends are mutually exclusive, so tokio and
smol need the default turned off:

```toml
# tokio
monocoque-rs = { version = "1.0", default-features = false, features = ["runtime-tokio", "zmq"] }

# smol
monocoque-rs = { version = "1.0", default-features = false, features = ["runtime-smol", "zmq"] }
```

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
would block, `TimedOut` when `recv_timeout` or `send_timeout` elapses, and
`HostUnreachable` for a send to an unknown peer on SERVER and STREAM. A ROUTER
drops a message for an unknown peer, as in libzmq, and fails with
`HostUnreachable` only when `router_mandatory` is set.

### Options

Options are a `SocketOptions` value, built with `with_*` methods and passed at
construction:

```rust
let mut socket = DealerSocket::with_options(
    SocketOptions::default()
        .with_recv_timeout(Duration::from_secs(5))
        .with_recv_hwm(1000),
);
```

They can be changed afterwards, like `zmq_setsockopt` on a live socket.
`update_options` changes some and keeps the rest; `set_options` replaces the
whole set. HWMs apply to existing peers at once, and connection settings apply
to connections made afterwards.

```rust
socket.update_options(|o| o.with_send_hwm(5000));
```

### poll, proxy, metadata

`monocoque::zmq::poll::poll` is `zmq_poll` over any mix of socket kinds.
`proxy` and `proxy_steerable` match libzmq's. A received message exposes its
connection's properties with `gets("Socket-Type")`, `gets("User-Id")` and so on,
like `zmq_msg_gets`.

### Checklist

- Replace `zmq` with `monocoque-rs` and its `zmq` feature; drop the system libzmq.
- Run socket code inside `LocalRuntime::block_on` (or your backend's runtime).
- Remove `Context`; create sockets directly.
- Replace `SNDMORE` sequences with one `Msg`.
- Replace setters with the `SocketOptions` builder, and `update_options` for
  changes on a live socket.
- Call `recv_owned()` wherever a received message is kept.

---

## From monocoque 0.4 to 1.0

1.0 replaces the per-socket I/O code with one multiplexer and one
message type. It is a single break with no compatibility shims.

### Construction

In 0.4 `connect` and `bind` were constructors: `connect` returned a socket
tied to one stream, and `bind` accepted one peer and returned the listener
with the socket. In 1.0 a socket is created first, with `new()` or
`with_options(opts)`, and `bind` and `connect` are methods on it that may be
called any number of times.

```rust
// 0.4
let mut dealer = DealerSocket::connect("tcp://127.0.0.1:5555").await?;
let (listener, mut router) = RouterSocket::bind("127.0.0.1:5555").await?;

// 1.0
let mut dealer = DealerSocket::new();
dealer.connect("tcp://127.0.0.1:5555").await?;

let mut router = RouterSocket::new();
let endpoint = router.bind("tcp://127.0.0.1:5555").await?;
```

`bind` takes an endpoint string, not a socket address, and returns the
endpoint it bound (with a port of 0 resolved). There is no listener to hold or
accept on: the socket accepts peers for as long as it lives, and `unbind`
stops it. The socket types are no longer generic over a stream type
(`DealerSocket<S>` is `DealerSocket`).

The other 0.4 constructors are removed:

| 0.4 | 1.0 |
|---|---|
| `connect_with_options(endpoint, opts)`, `bind_with_options(addr, opts)` | `with_options(opts)`, then `connect(endpoint)` or `bind(endpoint)` |
| `connect_ipc(path)` | `connect("ipc:///path")` |
| `from_tcp(stream)`, `from_unix_stream(stream)`, ROUTER `from_stream(stream)` | `new()`, then `attach(stream)` |
| `from_tcp_with_options(stream, opts)`, `from_unix_stream_with_options(stream, opts)`, `with_options(stream, opts)` | `with_options(opts)`, then `attach(stream)` |
| PUSH and PULL `new(stream)` | `new()`, then `attach(stream)` |

```rust
// 0.4
let mut sub = SubSocket::connect_ipc("/tmp/feed.sock").await?;
let mut rep = RepSocket::from_tcp_with_options(stream, opts).await?;

// 1.0
let mut sub = SubSocket::new();
sub.connect("ipc:///tmp/feed.sock").await?;

let mut rep = RepSocket::with_options(opts);
rep.attach(stream).await?;
```

`attach` takes the `TcpStream` and `UnixStream` types from `monocoque::rt`. A
stream added with `attach` is not redialed when it drops.

### Messages

- `Vec<Bytes>` is gone from every signature; `Msg` replaces it. There is no
  `From<Vec<Bytes>>` bridge. Build messages with `Msg::from(bytes)`,
  `Msg::from([a, b])`, `Msg::from("text")`, or collect an iterator of `Bytes`.
- `message_builder::Message` and the `message` module are removed.
- `recv()` returns `Result<MsgRef<'_>>` instead of `Result<Option<Vec<Bytes>>>`.
  Nothing returns `Ok(None)` any more. A timeout is the error `TimedOut` (or
  `Again` for a zero timeout). A closed connection is not reported by `recv`
  at all on most sockets: the endpoint is redialed and `recv` goes on waiting
  for a message from any peer. The exception is REQ, where a peer that goes
  away before replying fails the `recv` with `PeerDisconnected`. Code that
  keeps the message calls `recv_owned()`.
- `recv_into` and `try_recv_into` are removed; the borrowed `recv` supersedes
  them. `send_one` is removed; `send(Msg::from(frame))` no longer allocates.
- `recv_one` is removed. Take the first frame of a received message with
  `first()`, or with `frame_bytes(0)` to keep it as `Bytes`.
- `has_more` is removed. A receive returns a whole message, so there is never
  a part left to ask about.
- PULL's `try_recv` is removed. Check `has_message()` before `recv()`, or set
  `with_recv_timeout(Duration::ZERO)`, under which an empty `recv()` fails with
  `Again`.

```rust
// 0.4
if let Some(frames) = socket.recv().await? {
    handle(&frames[0]);
}
let first = socket.recv_one().await?;

// 1.0
let msg = socket.recv().await?;
handle(msg.first().unwrap_or_default());
let first = socket.recv().await?.frame_bytes(0);
```

### Batching

`send_buffered`, `send_batch`, `recv_batch` and `buffered_bytes` are removed.
Batched writes are a socket option now, write coalescing, and `send` is the
only send call:

```rust
// 0.4
for msg in batch {
    socket.send_buffered(msg)?;
}
socket.flush().await?;
// or
socket.send_batch(&batch).await?;

// 1.0
let mut socket = DealerSocket::with_options(
    SocketOptions::default().with_write_coalescing(true),
);
socket.connect("tcp://127.0.0.1:5555").await?;
for msg in batch {
    socket.send(msg).await?;
}
socket.flush().await?;
```

With coalescing on, `send` appends to a buffer that is written when it reaches
`write_coalesce_threshold` or on `flush()`. Nothing reports the bytes held in
it. In place of `recv_batch`, call `recv()` in a loop; `has_message()` says
whether the next one would return without waiting.

### Send does its work when called

`send` on PUSH, DEALER and PAIR, and `send` and `send_ref` on PUB, XPUB and
RADIO, do their work when called rather than when the returned future is first
polled. A future that is dropped without being polled has still sent. Code
that built a send future and dropped it to cancel the send, for example the
losing arm of a `select!` that was never polled, now sends the message.

```rust
let fut = push.send("job");   // 1.0: the message is already queued or written
drop(fut);                    // 0.4: nothing was sent
```

On PUB, XPUB and RADIO `send` returns `Result<usize>`, the number of
subscribers that took the message, instead of `Result<()>`.

### Reconnection

`try_reconnect`, `send_with_reconnect`, `recv_with_reconnect` and
`is_connected` are removed. Every endpoint passed to `connect` is redialed by
the socket whenever its connection drops, and `send` and `recv` keep working
across the drop, so the plain calls replace the `_with_reconnect` ones.
`peer_count()` says how many peers are connected at the moment.

```rust
// 0.4
socket.send_with_reconnect(msg).await?;
let reply = socket.recv_with_reconnect().await?;

// 1.0
socket.send(msg).await?;
let reply = socket.recv().await?;
```

### Errors

- Every public method returns `Result<_, MonocoqueError>`; nothing returns
  `io::Result`. I/O errors arrive as `MonocoqueError::Io`.
- `zmq::common::channel_to_io_error` is removed with the `io::Result` signatures.
- The public enums that will grow are `#[non_exhaustive]`, so a `match` on one
  needs a wildcard arm: `MonocoqueError`, `ReconnectError`, `SocketEvent`,
  `SocketType`, `Endpoint`, `EndpointError`, `SubscriptionEvent`,
  `ProxyCommand`, `ZapMechanism`, `ZapStatus`, `ZmtpError`, `CurveError` and
  `CloseReason`. None of them was in 0.4.

  ```rust
  // 0.4
  match event {
      SubscriptionEvent::Subscribe(prefix) => added(prefix),
      SubscriptionEvent::Unsubscribe(prefix) => removed(prefix),
  }

  // 1.0
  match event {
      SubscriptionEvent::Subscribe(prefix) => added(prefix),
      SubscriptionEvent::Unsubscribe(prefix) => removed(prefix),
      _ => {}
  }
  ```

### Sockets

- "bind accepts exactly one peer" is gone: every socket type is multi-peer, on
  bind and on connect, and may bind and connect several endpoints.
- `PushFanOut` and `PullFanIn` are removed; `PushSocket` and `PullSocket` load
  balance and fair-queue natively.
- `connect` returns once the dial is started. Sends wait for a peer (or fail
  with `Again`/`TimedOut` per the options), as in libzmq.
- `socket_type()` is a method on every socket (`socket.socket_type()`); on
  most 0.4 sockets it was an associated function.
- `last_endpoint()` returns `Option<String>`, the endpoint most recently
  bound, instead of a borrowed `&str` or `&Endpoint`.
- `events()` is removed. `has_message()` and `can_send()` answer the same two
  questions, and `monocoque::zmq::poll::poll` waits on several sockets.
- The crate-root re-export `monocoque::SocketType` is removed; the type is
  `monocoque::zmq::SocketType`.
- `monocoque::zmq::prelude` exports a different set. It no longer has
  `StreamSocket`, `Subscription`, `SubscriptionEvent` and `SubscriptionTrie`
  (import them from `monocoque::zmq`), `proxy`, `proxy_steerable`,
  `ProxyCommand` and `ProxySocket` (import them from `monocoque::zmq::proxy`),
  or the removed `BufferConfig`, `PushFanOut` and `PullFanIn`. It now has
  `Msg`, `MsgRef`, `MonocoqueError` and `Result`. That `Result` is
  `monocoque::zmq::Result<T>`, with one type parameter, and a glob import of
  the prelude puts it in place of the standard one: write
  `std::result::Result<T, E>` where a function in that module returns another
  error type.

### PUB

- PUB is eager by default, like every socket: `send()` puts the message into
  every matching subscriber's kernel buffer before it returns, one send per
  subscriber. 0.4's PUB queued messages to worker threads that batched them,
  so a publisher that relied on that throughput should turn on
  `with_write_coalescing(true)`.
- PUB's `send_frames(&[Bytes])` is gone. `send(msg)` takes a `Msg`, and
  `send_ref(&msg)` publishes one the caller keeps without cloning it; XPUB and
  RADIO have both.
- `bind_with_workers` and `bind_with_workers_opts` are removed. There are no
  worker threads: a PUB socket runs on the thread that owns it. Use
  `PubSocket::new()` or `with_options(opts)`, then `bind`.
- `accept_subscriber` is removed. Subscribers are accepted in the background
  from the moment `bind` returns.
- `subscriber_count()` is `peer_count()`.
- `local_addr()` is removed. `bind` returns the bound endpoint as a string,
  and `last_endpoint()` returns it later.
- `drop_count()` is removed and there is no drop counter. `send` returns how
  many subscribers took the message; a subscriber at its HWM is not counted.

```rust
// 0.4
let mut publisher = PubSocket::bind_with_workers("127.0.0.1:5556", 4).await?;
publisher.accept_subscriber().await?;
let port = publisher.local_addr()?.port();
publisher.send(vec![topic, body]).await?;

// 1.0
let mut publisher = PubSocket::new();
let endpoint = publisher.bind("tcp://127.0.0.1:5556").await?;
let delivered = publisher.send(Msg::from([topic, body])).await?;
```

### ROUTER

- `set_router_mandatory`, `set_router_handover` and `set_connect_routing_id`
  are removed. Set `with_router_mandatory`, `with_router_handover` and
  `with_connect_routing_id` on the options.
- `peer_identity()` is removed, since a ROUTER has many peers.
  `identities()` lists the identities of the connected peers and
  `identity_of(peer)` returns one peer's.

### XPUB

- `set_verbose` and `set_manual` are removed. Set `with_xpub_verbose` and
  `with_xpub_manual` on the options the socket is created with; both are read
  once, at construction.
- `accept()` is removed. Subscribers are accepted in the background from the
  moment `bind` returns.
- `subscriber_count()` is `peer_count()`.
- `local_addr()` is removed. `bind` returns the bound endpoint as a string,
  and `last_endpoint()` returns it later.
- `recv_subscription()` returns `Result<SubscriptionEvent>` instead of
  `io::Result<Option<SubscriptionEvent>>`. It waits for the next subscription
  change; a `recv_timeout` that elapses is the error `TimedOut`, not
  `Ok(None)`. `recv()` returns the same changes undecoded, as `\x01prefix` and
  `\x00prefix` messages.
- `connect_upstream` and `send_subscription` are removed; an XPUB no longer
  holds an upstream connection of its own. Create an `XSubSocket`, connect it
  to the upstream publisher, and pass each message the XPUB receives to
  `xsub.send(msg)`, or run the pair under `monocoque::zmq::proxy::proxy`. In
  manual mode, `subscribe(prefix)` and `unsubscribe(prefix)` on the XPUB
  decide what the peer that sent the last subscription message receives.

```rust
// 0.4
let mut xpub = XPubSocket::bind("127.0.0.1:5556").await?;
xpub.set_manual(true);
xpub.connect_upstream("127.0.0.1:5555").await?;
if let Some(event) = xpub.recv_subscription().await? {
    xpub.send_subscription(event).await?;
}

// 1.0
let mut xpub = XPubSocket::new();
xpub.bind("tcp://127.0.0.1:5556").await?;
let mut xsub = XSubSocket::new();
xsub.connect("tcp://127.0.0.1:5555").await?;
let change = xpub.recv_owned().await?;
xsub.send(change).await?;
```

### XSUB

- `send_subscription_event(event)` is removed. Call `subscribe(prefix)` or
  `unsubscribe(prefix)`, or `send` a subscription message as received from an
  XPUB (`\x01prefix` or `\x00prefix`); `send` remembers it and replays it to
  publishers that connect later.
- `is_subscribed`, `subscription_count` and `subscriptions` are removed from
  XSUB and have no replacement there; keep the list in the application if it
  is needed. `SubSocket` has `subscriptions()`, which returns the prefixes as
  `Vec<Bytes>`.

### PAIR

- `bind_inproc`, `connect_inproc` and their `_with_options` forms are removed.
  inproc is an endpoint like any other, and `bind` and `connect` are `async`:

```rust
// 0.4
let a = PairSocket::bind_inproc("inproc://pipe")?;
let b = PairSocket::connect_inproc_with_options("inproc://pipe", opts)?;

// 1.0
let mut a = PairSocket::new();
a.bind("inproc://pipe").await?;
let mut b = PairSocket::with_options(opts);
b.connect("inproc://pipe").await?;
```

### STREAM

- A message is two frames, `[identity, data]`, as in libzmq. 0.4 put an empty
  frame between them, on `recv` and on `send`. Identities are 5 bytes, not 8.
- `recv()` returns `Result<MsgRef<'_>>` instead of
  `io::Result<Option<Vec<Bytes>>>`, like every other socket.
- `accept_raw()` is removed. Connections are accepted in the background, and
  each one is announced by an `[identity, ""]` message from `recv` (unless
  `with_stream_notify(false)`), which is where the identity now comes from.
- `close_peer(routing_id)` and `disconnect(routing_id)` are removed. Close a
  connection by sending `[identity, ""]`; what is queued for it is written
  first. `disconnect` now takes an endpoint, as on every socket, and stops
  dialing it.
- `local_addr()` is removed. `bind` returns the bound endpoint as a string,
  and `last_endpoint()` returns it later.
- A send to an unknown identity fails with `HostUnreachable` instead of being
  dropped.
- `StreamSocket` has the methods the other sockets have: `connect`,
  `disconnect`, `unbind`, `attach`, `monitor`, `options`, `set_options`,
  `update_options`, `peer_ids`, `last_endpoint`, `peer_property`,
  `last_metadata`, `last_peer`, `has_message`, `can_send`, `recv_owned`,
  `flush` and `close`.

```rust
// 0.4
let mut stream = StreamSocket::bind("127.0.0.1:8080").await?;
let id = stream.accept_raw().await?;
stream.send(vec![id.clone(), Bytes::new(), reply]).await?;
stream.close_peer(&id);

// 1.0
let mut stream = StreamSocket::new();
stream.bind("tcp://127.0.0.1:8080").await?;
let id = stream.recv().await?.frame_bytes(0).unwrap();   // the connect notification
stream.send(Msg::from([id.clone(), reply])).await?;
stream.send(Msg::from([id, Bytes::new()])).await?;       // close it
```

### Options

- `SocketOptions` is `#[non_exhaustive]`. A struct literal, with or without
  `..Default::default()`, no longer compiles outside the crate. Start from
  `SocketOptions::default()` (or `new()`) and use the `with_*` methods.

  ```rust
  // 0.4
  let opts = SocketOptions { send_hwm: 5000, ..Default::default() };

  // 1.0
  let opts = SocketOptions::default().with_send_hwm(5000);
  ```

- `options_mut()` is removed, and `options()` returns a copy instead of a
  reference. Change a live socket's options with `update_options`, or replace
  them with `set_options`:

  ```rust
  // 0.4
  socket.options_mut().recv_timeout = Some(Duration::from_secs(5));

  // 1.0
  socket.update_options(|o| o.with_recv_timeout(Duration::from_secs(5)));
  ```

- These options and their `with_*` builders are removed. They were stored and
  never read, so removing a call that set one changes nothing at run time:
  `immediate`, `probe_router`, `router_raw`, `xpub_nodrop`, `ipv6`,
  `bind_to_device`, `rate`, `recovery_ivl`, `multicast_hops`,
  `multicast_maxtpdu`, and the `unsubscriptions` field with its builder
  `with_unsubscribe`. None has a replacement. To drop a subscription on a live
  socket, call `unsubscribe(prefix)` on it.
- `BufferConfig` and the `small()` and `large()` presets are removed. Set the
  read side with `with_read_buffer_size`.
- `monocoque_core::io::READ_SLAB_SIZE` is `MAX_READ_SIZE` (still 64 KiB, the
  ceiling `read_buffer_size` clamps to), and `io::take_read_buffer` is removed.
- `write_buffer_size`, `with_write_buffer_size` and `with_buffer_sizes` are
  removed; the outbound buffer grows on demand and is reused, so the setting
  had no effect. Use `with_read_buffer_size` for the read side.
- `tos` is a `u8` (it was an `i32`), and it and `connect_timeout` now take
  effect: `tos` sets the IPv4 TOS byte or IPv6 traffic class of each TCP
  connection, and a dial that takes longer than `connect_timeout` counts as a
  failed attempt. The default `connect_timeout` of zero leaves the limit to
  the OS.
- `max_reconnect_attempts`, `xpub_manual`, `xpub_welcome_msg` and
  `connect_routing_id` now take effect; they were accepted and ignored.
- `with_conflate()` now takes effect: a conflating socket keeps only the newest
  message. Code that set it and relied on it being ignored changes behaviour.
- `handshake_timeout` of zero means no limit, as in libzmq.
- `linger: None` waits until everything queued is written, however long that
  takes. `Some(Duration::ZERO)` drops what is still queued.
- `max_msg_size: None` is 64 MiB per frame, not unlimited. A frame above
  256 MiB is refused whatever the option says.
- A `recv_timeout` or `send_timeout` of zero fails with `MonocoqueError::Again`
  when the operation cannot complete at once. A non-zero timeout that elapses
  fails with `MonocoqueError::TimedOut`.
- `recv_hwm` applies to every socket type, not only STREAM.
- `hwm == 0` means unbounded, as in libzmq.
- `vectored_write_threshold` defaults to 16 KiB (was 32 KiB).

### inproc

- inproc is bounded by the HWM in each direction instead of unbounded, and it
  works across threads as well as within one.

### Security

- The security types are re-exported at `monocoque::zmq::security`, with the
  submodules `curve`, `plain`, `zap` and `zap_handler`. An application no
  longer depends on `monocoque-rs-zmtp` to name them.

  ```rust
  // 0.4
  use monocoque_zmtp::security::curve::CurveKeyPair;

  // 1.0
  use monocoque::zmq::security::curve::CurveKeyPair;
  ```

- CURVE now speaks RFC 26 and interoperates with libzmq. A 0.4 peer cannot
  complete a CURVE handshake with 1.0; upgrade both ends together.
- A CURVE server asks the ZAP handler about every client whenever a handler is
  running, with or without a ZAP domain. In 0.4 a server with no domain skipped
  ZAP. With no handler and no domain it still admits any client key.
- `with_curve_keypair` refuses a public key that does not belong to the secret
  key: the handshake fails instead of ignoring the public key.
- `ZapHandler` and `PlainAuthHandler` use native `async fn`. They can no longer
  be used as `dyn` trait objects; store a concrete type, for example
  `Arc<StaticPlainHandler>`. The `async-trait` dependency is gone.

### Logging

- The crates no longer enable `tracing`'s `release_max_level_info`, so `debug!`
  output from monocoque is available in release builds, and the application's
  own log level is no longer capped. The library installs no subscriber; see
  the Debugging section of the [User Guide](USER_GUIDE.md).

### Draft sockets

- CLIENT/SERVER, RADIO/DISH, SCATTER/GATHER and CHANNEL exist behind the
  `draft` feature.
