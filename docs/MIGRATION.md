# Migration Guide

**Migrating to monocoque from libzmq or zmq.rs**

**Last Updated**: January 25, 2026

---

## Table of Contents

1. [From libzmq (C/C++)](#from-libzmq)
2. [From zmq.rs](#from-zmqrs)
3. [API Mapping Reference](#api-mapping-reference)
4. [Common Patterns](#common-patterns)
5. [Breaking Changes](#breaking-changes)

---

## From libzmq

### Conceptual Differences

| Concept | libzmq | monocoque |
|---------|--------|-----------|
| **Runtime** | Blocking / callbacks | Async/await with compio |
| **Messages** | `zmq_msg_t` | `Vec<Bytes>` |
| **Context** | `zmq_ctx_t` (global) | No context (per-socket) |
| **Polling** | `zmq_poll()` | `futures::select!` |
| **Errors** | `errno` codes | `std::io::Error` |

### Socket Creation

**libzmq:**
```c
void *context = zmq_ctx_new();
void *socket = zmq_socket(context, ZMQ_DEALER);
zmq_connect(socket, "tcp://localhost:5555");
```

**monocoque:**
```rust
let socket = DealerSocket::from_tcp("tcp://localhost:5555").await?;
```

### Socket Options

**libzmq:**
```c
int timeout = 5000;  // milliseconds
zmq_setsockopt(socket, ZMQ_RCVTIMEO, &timeout, sizeof(timeout));

int hwm = 1000;
zmq_setsockopt(socket, ZMQ_RCVHWM, &hwm, sizeof(hwm));
```

**monocoque:**
```rust
use std::time::Duration;

let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000);
    
let socket = DealerSocket::with_options(options);
```

### Send/Receive

**libzmq:**
```c
// Send
char buf[] = "Hello";
zmq_send(socket, buf, strlen(buf), 0);

// Receive
char buffer[256];
int size = zmq_recv(socket, buffer, 256, 0);
```

**monocoque:**
```rust
// Send
socket.send(vec![Bytes::from("Hello")]).await?;

// Receive
if let Some(msg) = socket.recv().await? {
    println!("Received: {:?}", msg);
}
```

### Multipart Messages

**libzmq:**
```c
zmq_send(socket, "frame1", 6, ZMQ_SNDMORE);
zmq_send(socket, "frame2", 6, ZMQ_SNDMORE);
zmq_send(socket, "frame3", 6, 0);  // Last frame

// Check for more
int more;
size_t more_size = sizeof(more);
zmq_getsockopt(socket, ZMQ_RCVMORE, &more, &more_size);
```

**monocoque:**
```rust
// Send multipart
let msg = vec![
    Bytes::from("frame1"),
    Bytes::from("frame2"),
    Bytes::from("frame3"),
];
socket.send(msg).await?;

// All frames received together as Vec<Bytes>
let msg = socket.recv().await?;

// Check for more (in same logical message)
if socket.has_more() {
    // More frames available
}
```

### Security

**libzmq (PLAIN):**
```c
zmq_setsockopt(socket, ZMQ_PLAIN_SERVER, &server, sizeof(server));
zmq_setsockopt(socket, ZMQ_PLAIN_USERNAME, "admin", 5);
zmq_setsockopt(socket, ZMQ_PLAIN_PASSWORD, "secret", 6);
```

**monocoque:**
```rust
let options = SocketOptions::new()
    .with_plain_server(true)
    .with_plain_credentials("admin", "secret");
```

**libzmq (CURVE):**
```c
char server_public[32];
char server_secret[32];
zmq_curve_keypair(server_public, server_secret);

zmq_setsockopt(socket, ZMQ_CURVE_SERVER, &server, sizeof(server));
zmq_setsockopt(socket, ZMQ_CURVE_SECRETKEY, server_secret, 32);
```

**monocoque:**
```rust
let keypair = CurveKeyPair::generate();
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(
        *keypair.public.as_bytes(),
        *keypair.public.as_bytes()  // Store secret securely
    );
```

### Proxy/Device

**libzmq:**
```c
void *frontend = zmq_socket(ctx, ZMQ_ROUTER);
void *backend = zmq_socket(ctx, ZMQ_DEALER);

zmq_bind(frontend, "tcp://*:5559");
zmq_bind(backend, "tcp://*:5560");

zmq_proxy(frontend, backend, NULL);
```

**monocoque:**
```rust
let mut frontend = RouterSocket::from_tcp("tcp://*:5559").await?;
let mut backend = DealerSocket::from_tcp("tcp://*:5560").await?;

proxy(&mut frontend, &mut backend, None).await?;
```

---

## From zmq.rs

### High-Level Differences

| Feature | zmq.rs | monocoque |
|---------|--------|-----------|
| **Async** | Blocking by default | Async/await native |
| **Runtime** | Threads | compio (io_uring) |
| **Messages** | `Message` type | `Vec<Bytes>` |
| **Context** | `Context` required | No context needed |
| **Builder** | No | `SocketOptions` builder |

### Socket Creation

**zmq.rs:**
```rust
let context = zmq::Context::new();
let socket = context.socket(zmq::DEALER)?;
socket.connect("tcp://localhost:5555")?;
```

**monocoque:**
```rust
// Simpler - no context needed
let socket = DealerSocket::from_tcp("tcp://localhost:5555").await?;
```

### Send/Receive

**zmq.rs:**
```rust
// Send
socket.send("Hello", 0)?;

// Receive
let msg = socket.recv_msg(0)?;
let data = msg.as_str().unwrap();
```

**monocoque:**
```rust
// Send
socket.send(vec![Bytes::from("Hello")]).await?;

// Receive
let msg = socket.recv().await?;
let data = std::str::from_utf8(&msg[0])?;
```

### Multipart Messages

**zmq.rs:**
```rust
// Send
socket.send("frame1", zmq::SNDMORE)?;
socket.send("frame2", zmq::SNDMORE)?;
socket.send("frame3", 0)?;

// Receive
let mut msg = zmq::Message::new();
socket.recv(&mut msg, 0)?;
let more = msg.get_more();
```

**monocoque:**
```rust
// Send (all at once)
let msg = vec![
    Bytes::from("frame1"),
    Bytes::from("frame2"),
    Bytes::from("frame3"),
];
socket.send(msg).await?;

// Receive (all at once)
let msg = socket.recv().await?;
// msg is Vec<Bytes> with all frames
```

### Socket Options

**zmq.rs:**
```rust
socket.set_rcvtimeo(5000)?;  // milliseconds
socket.set_rcvhwm(1000)?;
socket.set_linger(100)?;
```

**monocoque:**
```rust
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000)
    .with_linger(Duration::from_millis(100));
    
let socket = DealerSocket::with_options(options);
```

### Async Usage

**zmq.rs:**
```rust
// Requires external async adapter
use async_zmq::*;

let dealer = async_zmq::dealer("tcp://localhost:5555")?;
dealer.send(vec!["Hello"]).await?;
let msg = dealer.recv().await?;
```

**monocoque:**
```rust
// Native async/await
let mut socket = DealerSocket::from_tcp("tcp://localhost:5555").await?;
socket.send(vec![Bytes::from("Hello")]).await?;
let msg = socket.recv().await?;
```

---

## API Mapping Reference

### Socket Types

| libzmq/zmq.rs | monocoque |
|---------------|-----------|
| `ZMQ_REQ` | `ReqSocket` |
| `ZMQ_REP` | `RepSocket` |
| `ZMQ_DEALER` | `DealerSocket` |
| `ZMQ_ROUTER` | `RouterSocket` |
| `ZMQ_PUB` | `PubSocket` |
| `ZMQ_SUB` | `SubSocket` |
| `ZMQ_XPUB` | `XPubSocket` |
| `ZMQ_XSUB` | `XSubSocket` |
| `ZMQ_PUSH` | `PushSocket` |
| `ZMQ_PULL` | `PullSocket` |
| `ZMQ_PAIR` | `PairSocket` |

### Socket Options

| libzmq Constant | monocoque Method |
|-----------------|------------------|
| `ZMQ_RCVTIMEO` | `.with_recv_timeout()` |
| `ZMQ_SNDTIMEO` | `.with_send_timeout()` |
| `ZMQ_RCVHWM` | `.with_recv_hwm()` |
| `ZMQ_SNDHWM` | `.with_send_hwm()` |
| `ZMQ_LINGER` | `.with_linger()` |
| `ZMQ_IMMEDIATE` | `.with_immediate()` |
| `ZMQ_CONFLATE` | `.with_conflate()` |
| `ZMQ_ROUTING_ID` | `.with_routing_id()` |
| `ZMQ_CONNECT_ROUTING_ID` | `.with_connect_routing_id()` |
| `ZMQ_ROUTER_MANDATORY` | `.with_router_mandatory()` |
| `ZMQ_TCP_KEEPALIVE` | `.with_tcp_keepalive()` |
| `ZMQ_TCP_KEEPALIVE_IDLE` | `.with_tcp_keepalive_idle()` |
| `ZMQ_PLAIN_SERVER` | `.with_plain_server()` |
| `ZMQ_PLAIN_USERNAME` | `.with_plain_credentials()` |
| `ZMQ_CURVE_SERVER` | `.with_curve_server()` |
| `ZMQ_CURVE_PUBLICKEY` | `.with_curve_keypair()` |
| `ZMQ_REQ_CORRELATE` | `.with_req_correlate()` |
| `ZMQ_REQ_RELAXED` | `.with_req_relaxed()` |

---

## Common Patterns

### Pattern 1: Request-Reply

**libzmq:**
```c
void *req = zmq_socket(ctx, ZMQ_REQ);
zmq_connect(req, "tcp://localhost:5555");
zmq_send(req, "Hello", 5, 0);
char buf[256];
zmq_recv(req, buf, 256, 0);
```

**monocoque:**
```rust
let mut req = ReqSocket::from_tcp("tcp://localhost:5555").await?;
req.send(vec![Bytes::from("Hello")]).await?;
let reply = req.recv().await?;
```

### Pattern 2: Publish-Subscribe

**libzmq:**
```c
void *sub = zmq_socket(ctx, ZMQ_SUB);
zmq_connect(sub, "tcp://localhost:5556");
zmq_setsockopt(sub, ZMQ_SUBSCRIBE, "weather", 7);

while (1) {
    char buf[256];
    zmq_recv(sub, buf, 256, 0);
}
```

**monocoque:**
```rust
let mut sub = SubSocket::from_tcp("tcp://localhost:5556").await?;
sub.subscribe(b"weather").await?;

while let Some(msg) = sub.recv().await? {
    println!("Update: {:?}", msg);
}
```

### Pattern 3: Pipeline

**libzmq:**
```c
void *push = zmq_socket(ctx, ZMQ_PUSH);
zmq_bind(push, "tcp://*:5557");
zmq_send(push, "task", 4, 0);
```

**monocoque:**
```rust
let mut push = PushSocket::from_tcp("tcp://*:5557").await?;
push.send(vec![Bytes::from("task")]).await?;
```

---

## Breaking Changes

### No Global Context

**Before (libzmq/zmq.rs):**
```rust
let ctx = zmq::Context::new();
let socket1 = ctx.socket(zmq::DEALER)?;
let socket2 = ctx.socket(zmq::ROUTER)?;
```

**After (monocoque):**
```rust
// Each socket is independent
let socket1 = DealerSocket::new();
let socket2 = RouterSocket::new();
```

### Async Everywhere

**Before (zmq.rs blocking):**
```rust
let msg = socket.recv_msg(0)?;  // Blocks thread
```

**After (monocoque async):**
```rust
let msg = socket.recv().await?;  // Async, yields to runtime
```

### No SNDMORE Flag

**Before (libzmq):**
```c
zmq_send(socket, "part1", 5, ZMQ_SNDMORE);
zmq_send(socket, "part2", 5, 0);
```

**After (monocoque):**
```rust
// Send all frames at once
socket.send(vec![
    Bytes::from("part1"),
    Bytes::from("part2"),
]).await?;
```

### Message Type

**Before (zmq.rs):**
```rust
let msg: zmq::Message = socket.recv_msg(0)?;
let bytes = msg.as_bytes();
```

**After (monocoque):**
```rust
let msg: Vec<Bytes> = socket.recv().await?.unwrap();
let bytes = &msg[0];
```

### Timeout Handling

**Before (libzmq errno):**
```c
if (zmq_recv(socket, buf, size, 0) == -1) {
    if (errno == EAGAIN) {
        // Timeout
    }
}
```

**After (monocoque Option):**
```rust
match socket.recv().await? {
    Some(msg) => println!("Got: {:?}", msg),
    None => println!("Timeout"),
}
```

---

## Migration Checklist

- [ ] Replace `zmq::Context::new()` with direct socket creation
- [ ] Convert blocking calls to `async/await`
- [ ] Change `zmq::Message` to `Vec<Bytes>`
- [ ] Replace SNDMORE flag with multipart vectors
- [ ] Update socket option calls to builder pattern
- [ ] Convert errno checks to `Result<Option<T>>` matching
- [ ] Replace `zmq_poll()` with `futures::select!`
- [ ] Update security configuration (PLAIN/CURVE)
- [ ] Add `#[compio::main]` to entry point
- [ ] Update dependencies in Cargo.toml

---

## Performance Considerations

### Memory Usage

**monocoque** uses `Bytes` for zero-copy message sharing:

```rust
let data = Bytes::from(vec![0u8; 1024]);

// Cheap clone (just ref count increment)
let data2 = data.clone();  // No copy!
```

### Runtime

**monocoque** uses compio (io_uring on Linux):

- Better CPU efficiency
- Higher throughput
- Lower latency for concurrent connections

### Buffer Sizes

Tune for your workload:

```rust
let options = SocketOptions::new()
    .with_read_buffer_size(64 * 1024)   // Large messages
    .with_write_buffer_size(64 * 1024);
```

---

## Getting Help

- **Examples**: See [examples/](../examples/) directory
- **User Guide**: [USER_GUIDE.md](USER_GUIDE.md)
- **API Docs**: https://docs.rs/monocoque
- **Issues**: https://github.com/vorjdux/monocoque/issues

---

**Last Updated**: January 25, 2026  
**License**: MIT OR Apache-2.0
