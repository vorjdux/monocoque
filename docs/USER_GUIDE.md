# Monocoque User Guide

Add monocoque to your `Cargo.toml`:

```toml
[dependencies]
monocoque-rs = "0.1"
bytes = "1.0"
compio = "0.12"
```

All socket operations are async and require a compio runtime. Annotate your entry point with `#[compio::main]`.

---

## Socket Types

Monocoque implements 11 ZeroMQ socket types. Pick based on your communication pattern:

- **REQ / REP** - synchronous request-reply. REQ sends then must receive before sending again; REP receives then must reply. Good for simple RPC.
- **DEALER / ROUTER** - async request-reply. DEALER can send multiple requests without waiting; ROUTER receives framed messages with a client identity prefix and can route replies back. Use these when you need concurrency or load balancing.
- **PUB / SUB** - one-to-many broadcast. SUB sockets filter by topic prefix. Publishers don't know or care who is subscribed.
- **XPUB / XSUB** - extended pub/sub with subscription visibility. XPUB delivers subscription/unsubscription events as messages, useful for building brokers.
- **PUSH / PULL** - pipeline. PUSH distributes tasks round-robin to connected PULL sockets. One-way, no replies.
- **PAIR** - exclusive point-to-point. Exactly one peer.

---

## Patterns

### Request-Reply (REQ/REP)

```rust
// Server
let (_listener, mut server) = RepSocket::bind("127.0.0.1:5555").await?;
loop {
    let request = server.recv().await.expect("connection closed");
    server.send(vec![Bytes::from("pong")]).await?;
}

// Client
let mut client = ReqSocket::connect("127.0.0.1:5555").await?;
client.send(vec![Bytes::from("ping")]).await?;
let reply = client.recv().await.expect("no reply");
```

REQ enforces strict send/recv alternation. If you need to fire multiple requests without waiting, use DEALER/ROUTER instead.

### Async Request-Reply (DEALER/ROUTER)

```rust
// Client
let mut client = DealerSocket::connect("127.0.0.1:5555").await?;
for i in 0..10 {
    client.send(vec![Bytes::from(format!("request {i}"))]).await?;
}

// Server
let (_listener, mut server) = RouterSocket::bind("127.0.0.1:5555").await?;
loop {
    let msg = server.recv().await.expect("connection closed");
    // msg[0] is the client identity, msg[1] is an empty delimiter, msg[2..] is the payload
    let reply = vec![msg[0].clone(), Bytes::new(), Bytes::from("ok")];
    server.send(reply).await?;
}
```

ROUTER prepends a client identity frame so you can route replies to specific clients. You must echo that identity back when replying.

### Publish-Subscribe

```rust
// Publisher
let mut pub_sock = PubSocket::bind("127.0.0.1:5556").await?;
pub_sock.send(vec![Bytes::from("weather"), Bytes::from("72F")]).await?;

// Subscriber
let mut sub_sock = SubSocket::connect("127.0.0.1:5556").await?;
sub_sock.subscribe(b"weather").await?;
while let Ok(Some(msg)) = sub_sock.recv().await {
    println!("{:?}", msg);
}
```

The first frame is the topic. SUB filters by prefix match against subscribed topics. Subscribe to `b""` to receive everything.

### Pipeline (PUSH/PULL)

```rust
// Ventilator - distributes work
let (_listener, mut push) = PushSocket::bind("127.0.0.1:5557").await?;
for i in 0..100 {
    push.send(vec![Bytes::from(format!("task {i}"))]).await?;
}

// Worker - pulls tasks, pushes results
let mut pull = PullSocket::connect("127.0.0.1:5557").await?;
let mut push = PushSocket::connect("127.0.0.1:5558").await?;
while let Ok(Some(task)) = pull.recv().await {
    push.send(process(task)).await?;
}
```

PUSH distributes messages round-robin across connected PULL sockets. There's no reply path - use a separate PULL socket to collect results if needed.

---

## Messages

All messages are `Vec<Bytes>`. A single-frame message is `vec![Bytes::from("hello")]`. Multipart messages are just more elements in the vec - you send all frames together and receive all frames together, no flags needed.

```rust
// ROUTER envelope: identity + empty delimiter + payload
let reply = vec![identity.clone(), Bytes::new(), Bytes::from("ok")];
```

`Bytes` is reference-counted, so cloning frames is cheap - no copying the underlying data.

---

## Socket Options

Pass a `SocketOptions` to `connect_with_options` or `bind_with_options`:

```rust
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000)
    .with_send_hwm(1000);

let socket = DealerSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

Key options:

- `recv_timeout` / `send_timeout` - how long to wait before returning `None`. Defaults to no timeout (wait forever).
- `recv_hwm` / `send_hwm` - high water marks. When the queue reaches this many messages, new messages are dropped or the sender blocks depending on socket type. Default 1000.
- `linger` - how long to wait for queued messages to drain when a socket closes. Default 0 (discard immediately).
- `conflate` - keep only the most recent message in the receive queue. Useful for telemetry or status updates where stale data is useless.
- `tcp_keepalive` - detect dead connections. Use `with_tcp_keepalive(1)`, `with_tcp_keepalive_idle(60)`, `with_tcp_keepalive_intvl(10)`, `with_tcp_keepalive_cnt(3)` to enable.

Buffer size presets: `SocketOptions::small()` (4KB, good for low-latency REQ/REP) and `SocketOptions::large()` (16KB, good for high-throughput DEALER/ROUTER).

---

## Error Handling

`recv()` returns `Result<Option<Vec<Bytes>>>`. An `Err` is an I/O error. `Ok(None)` means the connection closed or a timeout elapsed. `Ok(Some(msg))` is a message.

```rust
match socket.recv().await {
    Ok(Some(msg)) => { /* handle message */ }
    Ok(None) => { /* timeout or closed */ }
    Err(e) => { /* I/O error */ }
}
```

`send()` returns `Result<()>`. Errors indicate connection or buffer problems.

Set a recv timeout if you need to detect hangs rather than waiting forever.

---

## Security

PLAIN sends credentials in the clear - only use it over an already-encrypted channel.

```rust
// Server
let options = SocketOptions::new().with_plain_server(true);
let (_listener, mut server) = RepSocket::bind_with_options("127.0.0.1:5555", options).await?;

// Client
let options = SocketOptions::new().with_plain_credentials("admin", "secret");
let mut client = ReqSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

CURVE provides public-key encryption with forward secrecy. Generate keypairs with `CurveKeyPair::generate()`. Store secret keys outside of source code. See [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for a complete setup example.

---

## Proxies

Forward messages between two sockets:

```rust
let (_listener, mut frontend) = RouterSocket::bind("127.0.0.1:5559").await?;
let (_listener2, mut backend) = DealerSocket::bind("127.0.0.1:5560").await?;

proxy::proxy(&mut frontend, &mut backend, Option::<&mut RouterSocket>::None).await?;
```

For a steerable proxy, pass a PAIR socket as the third argument and send `b"PAUSE"`, `b"RESUME"`, or `b"TERMINATE"` to control it.

---

## Debugging

Enable debug logging with `RUST_LOG=monocoque_zmtp=debug`. Socket introspection at runtime:

```rust
socket.socket_type();     // which socket type
socket.last_endpoint();   // last bound/connected address
socket.has_more();        // more frames pending in current message
socket.options();         // current option values
```

---

See [examples/](../examples/) for complete working programs, [MIGRATION.md](MIGRATION.md) if you're coming from libzmq or rust-zmq, and [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for production security setup.
