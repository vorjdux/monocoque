# Monocoque User Guide

Add monocoque to your `Cargo.toml`:

```toml
[dependencies]
monocoque-rs = "0.4"
bytes = "1.0"
compio = { version = "0.19", features = ["runtime", "macros"] }
```

All socket operations are async. On the default backend you run them on a compio
runtime; annotate your entry point with `#[compio::main]`. Two portable backends
are available instead: build with `default-features = false, features =
["runtime-tokio", "zmq"]` to drive the sockets from a current-thread tokio runtime
inside a `LocalSet`, or with `default-features = false, features = ["runtime-smol",
"zmq"]` to drive them on a single-threaded smol executor. See
[Getting Started](GETTING_STARTED.md#choosing-a-runtime) for each setup.

---

## Socket Types

Monocoque implements 11 ZeroMQ socket types. Pick based on your communication pattern:

- **REQ / REP** - synchronous request-reply. REQ sends then must receive before sending again; REP receives then must reply. Good for simple RPC.
- **DEALER / ROUTER** - async request-reply. DEALER can send multiple requests without waiting; ROUTER receives framed messages with a client identity prefix and can route replies back. Use these when you need concurrency or load balancing.
- **PUB / SUB** - one-to-many broadcast. SUB sockets filter by topic prefix. Publishers don't know or care who is subscribed.
- **XPUB / XSUB** - extended pub/sub with subscription visibility. XPUB delivers subscription/unsubscription events as messages, useful for building brokers.
- **PUSH / PULL** - pipeline. PUSH distributes tasks round-robin to connected PULL sockets. One-way, no replies.
- **PAIR** - exclusive point-to-point. Exactly one peer.

Every socket is multi-peer: it can bind and connect several endpoints (`tcp://`, `ipc://`, `inproc://`). inproc is bounded by the HWM and works across threads. STREAM (raw TCP) is also available, and the draft types (CLIENT/SERVER, RADIO/DISH, SCATTER/GATHER, CHANNEL) are in `monocoque::zmq::draft` behind the `draft` feature.

---

## Patterns

### Request-Reply (REQ/REP)

```rust
// Server
let mut server = RepSocket::new();
server.bind("tcp://127.0.0.1:5555").await?;
loop {
    let _request = server.recv().await?;
    server.send("pong").await?;
}

// Client
let mut client = ReqSocket::new();
client.connect("tcp://127.0.0.1:5555").await?;
client.send("ping").await?;
let reply = client.recv().await?;
```

REQ enforces strict send/recv alternation. If you need to fire multiple requests without waiting, use DEALER/ROUTER instead.

In a hot loop the round trip above allocates nothing: `send(Msg::from(frame))` does not allocate, and `recv()` returns a `MsgRef` borrowed from a message slot the socket reuses. Call `recv_owned()` when you need to keep the message past the next receive.

### Async Request-Reply (DEALER/ROUTER)

```rust
// Client
let mut client = DealerSocket::new();
client.connect("tcp://127.0.0.1:5555").await?;
for i in 0..10 {
    client.send(format!("request {i}")).await?;
}

// Server
let mut server = RouterSocket::new();
server.bind("tcp://127.0.0.1:5555").await?;
loop {
    let msg = server.recv().await?;
    // frame 0 is the client identity, the rest is what the DEALER sent
    let identity = msg.frame_bytes(0).unwrap();
    server.send(Msg::from([identity, Bytes::from("ok")])).await?;
}
```

ROUTER prepends a client identity frame so you can route replies to specific clients. You must echo that identity back when replying.

### Publish-Subscribe

```rust
// Publisher
let mut pub_sock = PubSocket::new();
pub_sock.bind("tcp://127.0.0.1:5556").await?;
pub_sock.send(Msg::from([Bytes::from("weather"), Bytes::from("72F")])).await?;

// Subscriber
let mut sub_sock = SubSocket::new();
sub_sock.connect("tcp://127.0.0.1:5556").await?;
sub_sock.subscribe(b"weather").await?;
while let Ok(msg) = sub_sock.recv().await {
    println!("{:?}", msg);
}
```

A PUB socket serves every subscriber that connects. Publishers pushing many
small frames usually want write coalescing on:

```rust
let opts = SocketOptions::default().with_write_coalescing(true);
let mut pub_sock = PubSocket::with_options(opts);
pub_sock.bind("tcp://127.0.0.1:5556").await?;
```

The first frame is the topic. SUB filters by prefix match against subscribed topics. Subscribe to `b""` to receive everything.

### Pipeline (PUSH/PULL)

```rust
// Ventilator - distributes work
let mut push = PushSocket::new();
push.bind("tcp://127.0.0.1:5557").await?;
for i in 0..100 {
    push.send(format!("task {i}")).await?;
}

// Worker - pulls tasks, pushes results
let mut pull = PullSocket::new();
pull.connect("tcp://127.0.0.1:5557").await?;
let mut push = PushSocket::new();
push.connect("tcp://127.0.0.1:5558").await?;
while let Ok(task) = pull.recv().await {
    push.send(process(&task)).await?;
}
```

There's no reply path; use a separate PULL socket to collect results if needed.

### Worker pools

`PushSocket` and `PullSocket` are multi-peer. A bound PUSH socket round-robins
each `send` across every connected PULL worker, and a bound PULL socket
fair-queues the results of every connected PUSH worker into one stream. PUSH
only balances across peers that are already connected, so wait for the pool
(for example with `peer_count()`) before handing out work. See
`examples/pipeline_worker_pool.rs` for a full ventilator-workers-sink run.

---

## Messages

All messages are `Msg`. A single-frame message is `Msg::from("hello")` (or from `Bytes`, `String`, `Vec<u8>`); `send` takes anything that converts into `Msg`, so `send("hello")` works too. A multipart message is `Msg::from([a, b])` or an iterator of `Bytes` collected into a `Msg` - you send all frames together and receive all frames together, no flags needed.

```rust
// ROUTER envelope to a REQ peer: identity + empty delimiter + payload
let reply = Msg::from([identity.clone(), Bytes::new(), Bytes::from("ok")]);
```

`recv()` returns a `MsgRef`, which derefs to `Msg`: read frames with `get(i)`, `first()` or `frames()`, and take one as `Bytes` with `frame_bytes(i)`. `recv_owned()` returns the `Msg` itself.

`Bytes` is reference-counted, so cloning frames is cheap - no copying the underlying data.

---

## Socket Options

Pass a `SocketOptions` to `with_options` when creating the socket, or change them later with `set_options`:

```rust
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000)
    .with_send_hwm(1000);

let mut socket = DealerSocket::with_options(options);
socket.connect("tcp://127.0.0.1:5555").await?;
```

Key options:

- `recv_timeout` / `send_timeout` - how long to wait before failing with `MonocoqueError::TimedOut`. Defaults to no timeout (wait forever).
- `recv_hwm` / `send_hwm` - high water marks, applied to every socket type and transport, inproc included. When the queue reaches this many messages, new messages are dropped or the sender blocks depending on socket type. Default 1000; 0 means unbounded.
- `linger` - how long to wait for queued messages to drain when a socket closes. Default 30 seconds.
- `conflate` - keep only the most recent message in the receive queue. Useful for telemetry or status updates where stale data is useless.
- `tcp_keepalive` - detect dead connections. Use `with_tcp_keepalive(1)`, `with_tcp_keepalive_idle(60)`, `with_tcp_keepalive_intvl(10)`, `with_tcp_keepalive_cnt(3)` to enable.

Buffer sizing: the default read buffer is 32KB. The outbound buffer has no fixed size; it grows on demand and is reused across sends. Use `with_read_buffer_size(4096)` to trade throughput for latency on small messages, or `with_read_buffer_size(65_536)` to widen the read batch for bulk transfers. The read buffer is clamped to 64KB and floored at 64 bytes.

---

## Error Handling

Every method returns `Result<_, MonocoqueError>`. `recv()` returns `Result<MsgRef<'_>>`: `Ok(msg)` is a message, and a timeout or a closed socket is an error, not an empty result. I/O errors arrive as `MonocoqueError::Io`.

```rust
match socket.recv().await {
    Ok(msg) => { /* handle message */ }
    Err(MonocoqueError::TimedOut) => { /* recv timeout elapsed */ }
    Err(e) => { /* closed, I/O or protocol error */ }
}
```

`send()` returns `Result<()>`. Errors indicate connection or buffer problems; a send that cannot proceed fails with `Again` or `TimedOut` depending on the options.

Set a recv timeout if you need to detect hangs rather than waiting forever.

---

## Security

PLAIN sends credentials in the clear - only use it over an already-encrypted channel.

```rust
// Server
let options = SocketOptions::new().with_plain_server(true);
let mut server = RepSocket::with_options(options);
server.bind("tcp://127.0.0.1:5555").await?;

// Client
let options = SocketOptions::new().with_plain_credentials("admin", "secret");
let mut client = ReqSocket::with_options(options);
client.connect("tcp://127.0.0.1:5555").await?;
```

CURVE provides public-key encryption with forward secrecy. It implements RFC 26 and interoperates with libzmq peers. Generate keypairs with `monocoque_zmtp::security::curve::CurveKeyPair::generate()`. Store secret keys outside of source code. See [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for a complete setup example.

---

## Proxies

Forward messages between two sockets:

```rust
use monocoque::zmq::proxy::{self, NoCapture};

let mut frontend = RouterSocket::new();
frontend.bind("tcp://127.0.0.1:5559").await?;
let mut backend = DealerSocket::new();
backend.bind("tcp://127.0.0.1:5560").await?;

proxy::proxy(&mut frontend, &mut backend, None::<&mut NoCapture>).await?;
```

For a steerable proxy, call `proxy::proxy_steerable` with a control socket (for example PAIR) as the fourth argument and send `b"PAUSE"`, `b"RESUME"`, or `b"TERMINATE"` to control it.

---

## Polling

`monocoque::zmq::poll::poll` is the equivalent of `zmq_poll`: it waits on several sockets of any kind at once. See `examples/poll_demo.rs`.

---

## Debugging

Enable debug logging with `RUST_LOG=monocoque_zmtp=debug`. Socket introspection at runtime:

```rust
socket.socket_type();     // which socket type
socket.last_endpoint();   // last bound/connected address
socket.peer_count();      // connected peers
socket.options();         // current option values
```

---

See [examples/](../examples/) for complete working programs, [MIGRATION.md](MIGRATION.md) if you're coming from libzmq or rust-zmq, and [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for production security setup.
