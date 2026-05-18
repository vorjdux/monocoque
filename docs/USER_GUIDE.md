# Monocoque User Guide

**Complete guide to building ZeroMQ applications with monocoque**

**Version**: 0.1.0  
**Last Updated**: May 2026

---

## Table of Contents

1. [Getting Started](#getting-started)
2. [Core Concepts](#core-concepts)
3. [Socket Patterns](#socket-patterns)
4. [Advanced Features](#advanced-features)
5. [Best Practices](#best-practices)
6. [Performance Tuning](#performance-tuning)
7. [Security](#security)
8. [Troubleshooting](#troubleshooting)

---

## Getting Started

### Installation

Add monocoque to your `Cargo.toml`:

```toml
[dependencies]
monocoque = "0.1"
bytes = "1.0"
compio = "0.12"
```

### Your First Application

#### Simple REQ/REP Client-Server

**Server** (REP socket):
```rust
use monocoque::zmq::RepSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    // Bind and wait for the first client connection
    let (_listener, mut server) = RepSocket::bind("127.0.0.1:5555").await?;
    
    loop {
        // Receive request
        let request = server.recv().await.expect("Connection closed");
        println!("Received: {:?}", request);
        
        // Send reply
        let reply = vec![Bytes::from("World")];
        server.send(reply).await?;
    }
}
```

**Client** (REQ socket):
```rust
use monocoque::zmq::ReqSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    // Connect to server
    let mut client = ReqSocket::connect("127.0.0.1:5555").await?;
    
    // Send request
    let request = vec![Bytes::from("Hello")];
    client.send(request).await?;
    
    // Receive reply
    let reply = client.recv().await.expect("No reply");
    println!("Reply: {:?}", reply);
    
    Ok(())
}
```

---

## Core Concepts

### Socket Types

Monocoque implements 11 ZeroMQ socket types:

| Socket | Pattern | Direction | Use Case |
|--------|---------|-----------|----------|
| **REQ** | Request-Reply | Client | Synchronous RPC calls |
| **REP** | Request-Reply | Server | Synchronous RPC responses |
| **DEALER** | Request-Reply | Client | Async RPC, load balancing |
| **ROUTER** | Request-Reply | Server | Async routing, addressing |
| **PUB** | Publish-Subscribe | Publisher | Broadcasting events |
| **SUB** | Publish-Subscribe | Subscriber | Receiving filtered events |
| **XPUB** | Publish-Subscribe | Publisher | Pub with subscription events |
| **XSUB** | Publish-Subscribe | Subscriber | Sub with forwarding |
| **PUSH** | Pipeline | Producer | Task distribution |
| **PULL** | Pipeline | Consumer | Task collection |
| **PAIR** | Exclusive Pair | Bidirectional | Point-to-point communication |

### Message Model

All messages in monocoque are **multipart messages** represented as `Vec<Bytes>`:

```rust
use bytes::Bytes;

// Single-frame message
let msg = vec![Bytes::from("Hello")];

// Multi-frame message (envelope pattern)
let msg = vec![
    Bytes::from(""),           // Delimiter
    Bytes::from("topic"),      // Header
    Bytes::from("payload"),    // Body
];
```

#### Why `Bytes`?

- **Zero-copy**: Sharing messages is cheap (reference counting)
- **Immutable**: Thread-safe, no data races
- **Efficient**: No unnecessary allocations

### Socket Options

Configure socket behavior with `SocketOptions`:

```rust
use monocoque::zmq::{DealerSocket, SocketOptions};
use std::time::Duration;

let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000)
    .with_send_hwm(1000);

let socket = DealerSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

#### Common Options

| Option | Description | Default |
|--------|-------------|---------|
| `recv_timeout` | Max time to wait for recv | None (forever) |
| `send_timeout` | Max time to wait for send | None (forever) |
| `recv_hwm` | Receive high water mark | 1000 |
| `send_hwm` | Send high water mark | 1000 |
| `immediate` | Connect before handshake | false |
| `conflate` | Keep only latest message | false |
| `linger` | Close linger time | 0 |

---

## Socket Patterns

### 1. Request-Reply Pattern

#### Synchronous (REQ/REP)

**Use When:**
- Simple RPC calls
- Strict request-response ordering
- Single outstanding request

**Example:**
```rust
// Client
let mut client = ReqSocket::connect("127.0.0.1:5555").await?;
client.send(vec![Bytes::from("ping")]).await?;
let response = client.recv().await;

// Server
let (_listener, mut server) = RepSocket::bind("127.0.0.1:5555").await?;
let request = server.recv().await;
server.send(vec![Bytes::from("pong")]).await?;
```

#### Asynchronous (DEALER/ROUTER)

**Use When:**
- Multiple outstanding requests
- Load balancing across workers
- Parallel request processing

**Example:**
```rust
// Client (DEALER)
let mut client = DealerSocket::connect("127.0.0.1:5555").await?;
for i in 0..10 {
    client.send(vec![Bytes::from(format!("Request {}", i))]).await?;
}

// Server (ROUTER)
let (_listener, mut server) = RouterSocket::bind("127.0.0.1:5555").await?;
loop {
    let msg = server.recv().await.expect("connection closed");
    // msg[0] = client identity
    // msg[1] = empty delimiter
    // msg[2..] = request frames
    
    let reply = vec![
        msg[0].clone(),      // Return to sender
        Bytes::new(),        // Delimiter
        Bytes::from("OK"),   // Response
    ];
    server.send(reply).await?;
}
```

### 2. Publish-Subscribe Pattern

#### Basic PUB/SUB

**Use When:**
- Broadcasting to multiple subscribers
- Event distribution
- One-to-many communication

**Example:**
```rust
// Publisher
let mut publisher = PubSocket::bind("127.0.0.1:5556").await?;
loop {
    let event = vec![
        Bytes::from("weather"),              // Topic
        Bytes::from("temperature: 72°F"),    // Data
    ];
    publisher.send(event).await?;
    compio::time::sleep(Duration::from_secs(1)).await;
}

// Subscriber
let mut subscriber = SubSocket::connect("127.0.0.1:5556").await?;
subscriber.subscribe(b"weather").await?;
while let Ok(Some(msg)) = subscriber.recv().await {
    println!("Weather update: {:?}", msg);
}
```

#### Extended XPUB/XSUB

**Use When:**
- Building message brokers
- Dynamic subscription forwarding
- Monitoring subscriptions

**Example:**
```rust
use monocoque::zmq::{XSubSocket, XPubSocket, proxy};

// Broker (proxy pattern): publishers connect to XSUB, subscribers connect to XPUB
let mut frontend = XSubSocket::connect("127.0.0.1:5559").await?;
let mut backend = XPubSocket::bind("127.0.0.1:5560").await?;

proxy::proxy(&mut frontend, &mut backend, Option::<&mut XSubSocket>::None).await?;
```

### 3. Pipeline Pattern

#### PUSH/PULL

**Use When:**
- Distributing tasks to workers
- Parallel processing
- One-way data flow

**Example:**
```rust
// Ventilator (task producer) — binds, workers connect to it
let (_listener, mut ventilator) = PushSocket::bind("127.0.0.1:5557").await?;
for i in 0..100 {
    ventilator.send(vec![Bytes::from(format!("Task {}", i))]).await?;
}

// Worker — connects to ventilator and sink
let mut receiver = PullSocket::connect("127.0.0.1:5557").await?;
let mut sender = PushSocket::connect("127.0.0.1:5558").await?;
while let Ok(Some(task)) = receiver.recv().await {
    // Process task
    let result = process(task);
    sender.send(result).await?;
}

// Sink (result collector) — binds, workers connect to it
let (_listener, mut sink) = PullSocket::bind("127.0.0.1:5558").await?;
for _ in 0..100 {
    if let Ok(Some(result)) = sink.recv().await {
        println!("Result: {:?}", result);
    }
}
```

---

## Advanced Features

### Security

#### PLAIN Authentication

Username/password authentication:

```rust
use monocoque::zmq::{RepSocket, ReqSocket, SocketOptions};

// Server — enable PLAIN server mode, then bind
let options = SocketOptions::new().with_plain_server(true);
let (_listener, mut server) = RepSocket::bind_with_options("127.0.0.1:5555", options).await?;

// Client — attach credentials, then connect
let options = SocketOptions::new()
    .with_plain_credentials("admin", "secret123");
let mut client = ReqSocket::connect_with_options("127.0.0.1:5555", options).await?;
```

#### CURVE Encryption

Elliptic curve encryption with perfect forward secrecy:

```rust
use monocoque_zmtp::security::curve::CurveKeyPair;

// Server
let server_keypair = CurveKeyPair::generate();
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(
        *server_keypair.public.as_bytes(),
        // In production, securely store secret key
        *server_keypair.public.as_bytes()
    );

// Client
let client_keypair = CurveKeyPair::generate();
let options = SocketOptions::new()
    .with_curve_keypair(
        *client_keypair.public.as_bytes(),
        *client_keypair.public.as_bytes()
    )
    .with_curve_serverkey(*server_keypair.public.as_bytes());
```

See [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for complete security documentation.

### Socket Introspection

Query socket state at runtime:

```rust
// Get socket type
let socket_type = socket.socket_type();

// Get last endpoint
if let Some(endpoint) = socket.last_endpoint() {
    println!("Connected to: {}", endpoint);
}

// Check for more frames
if socket.has_more() {
    // More frames in current message
}

// Access options
let options = socket.options();
println!("Recv timeout: {:?}", options.recv_timeout);
```

### Message Proxies

Forward messages between socket pairs:

```rust
use monocoque::zmq::{proxy, DealerSocket, RouterSocket};

let (_listener, mut frontend) = RouterSocket::bind("127.0.0.1:5559").await?;
let (_listener2, mut backend) = DealerSocket::bind("127.0.0.1:5560").await?;

// Bidirectional forwarding
proxy::proxy(&mut frontend, &mut backend, Option::<&mut RouterSocket>::None).await?;
```

Steerable proxy with control socket:

```rust
use monocoque::zmq::{proxy, PairSocket};

let mut control = PairSocket::connect("127.0.0.1:5561").await?;

proxy::proxy_steerable(&mut frontend, &mut backend, Option::<&mut RouterSocket>::None, &mut control).await?;

// From another thread:
// Send b"PAUSE", b"RESUME", b"TERMINATE", or b"STATISTICS"
```

---

## Best Practices

### 1. Error Handling

Always handle potential errors:

```rust
// Good: Handle connection failures
match DealerSocket::connect("127.0.0.1:5555").await {
    Ok(mut socket) => {
        // Use socket
    }
    Err(e) => {
        eprintln!("Failed to connect: {}", e);
        // Retry with backoff, or fail gracefully
    }
}

// Good: Handle recv timeouts
let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5));
let mut socket = ReqSocket::connect_with_options("127.0.0.1:5555", options).await?;

match socket.recv().await {
    Some(msg) => println!("Received: {:?}", msg),
    None => println!("Connection closed or timed out"),
}
```

### 2. Resource Management

Use RAII for automatic cleanup:

```rust
{
    let socket = DealerSocket::connect("127.0.0.1:5555").await?;
    // Use socket
} // Socket automatically closed here
```

Set linger for graceful shutdown:

```rust
let options = SocketOptions::new()
    .with_linger(Duration::from_millis(100));
```

### 3. Message Construction

Use `Vec<Bytes>` directly for simple messages, or the builder for multi-frame envelopes:

```rust
use bytes::Bytes;

// Direct construction (most common)
let msg = vec![Bytes::from("topic"), Bytes::from("payload")];
socket.send(msg).await?;

// ROUTER envelope pattern (identity + delimiter + payload)
let reply = vec![
    identity.clone(),   // Routing identity
    Bytes::new(),       // Empty delimiter
    Bytes::from("OK"),  // Payload
];
socket.send(reply).await?;
```

### 4. High Water Marks

Prevent unbounded memory growth:

```rust
let options = SocketOptions::new()
    .with_recv_hwm(1000)  // Drop messages after 1000 queued
    .with_send_hwm(1000); // Block or drop after 1000 queued
```

### 5. TCP Keepalive

Detect broken connections:

```rust
use std::time::Duration;

let options = SocketOptions::new()
    .with_tcp_keepalive(1)  // Enable
    .with_tcp_keepalive_idle(60)  // Start after 60s idle
    .with_tcp_keepalive_intvl(10)  // Probe every 10s
    .with_tcp_keepalive_cnt(3);    // Give up after 3 probes
```

---

## Performance Tuning

### Buffer Sizes

Adjust for your message sizes:

```rust
let options = SocketOptions::new()
    .with_buffer_sizes(16 * 1024, 16 * 1024); // 16KB read + write buffers
```

Presets for common cases:

```rust
let options = SocketOptions::small();  // 4KB buffers (low latency REQ/REP)
let options = SocketOptions::large();  // 16KB buffers (high throughput DEALER/ROUTER)
```

### Conflation (Latest Value Cache)

Keep only the most recent message:

```rust
let options = SocketOptions::new()
    .with_conflate(true);  // Discard old messages

// Useful for:
// - Telemetry data
// - Status updates
// - "Last known good" caching
```

### Zero-Copy Operations

Leverage `Bytes` reference counting:

```rust
let data = Bytes::from(vec![0u8; 1024]);

// Cheap clone (just increments ref count)
let data2 = data.clone();
let data3 = data.clone();
```

### Batching

Process messages in batches:

```rust
let mut batch = Vec::new();
for _ in 0..100 {
    if let Some(msg) = socket.recv().await? {
        batch.push(msg);
    }
}
// Process entire batch at once
```

---

## Security

### Authentication Options

1. **NULL** (default): No authentication
2. **PLAIN**: Username/password (use over TLS!)
3. **CURVE**: Public-key encryption

### Security Checklist

- [ ] Never use PLAIN without TLS in production
- [ ] Rotate CURVE keys regularly
- [ ] Store secret keys securely (not in code)
- [ ] Use ZAP domain isolation
- [ ] Set appropriate socket timeouts
- [ ] Validate message sizes
- [ ] Sanitize user input in messages

See [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for detailed security best practices.

---

## Troubleshooting

### Common Issues

#### "Connection refused"
- Check server is running and listening
- Verify firewall rules
- Confirm correct port number
- Use `tcp://*:PORT` for bind, `tcp://HOST:PORT` for connect

#### "Resource temporarily unavailable"
- Socket buffer full (check HWM settings)
- Slow consumer (add backpressure handling)
- Consider conflate option for latest-value semantics

#### Messages not received
- Check topic filters on SUB sockets
- Verify socket types match (REQ↔REP, DEALER↔ROUTER)
- Add recv timeout to detect hangs
- Check for message ordering requirements

#### Memory leak
- Ensure sockets are properly dropped
- Set appropriate HWM limits
- Check for infinite loops holding messages
- Use `linger` for graceful shutdown

### Debugging Tips

Enable logging:

```rust
env_logger::init();  // Set RUST_LOG=monocoque_zmtp=debug
```

Add tracing:

```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = "0.3"
```

```rust
tracing::debug!("Sending message: {:?}", msg);
```

Use socket introspection:

```rust
println!("Socket type: {:?}", socket.socket_type());
println!("Last endpoint: {:?}", socket.last_endpoint());
println!("Options: {:#?}", socket.options());
```

---

## Next Steps

- Read [MIGRATION.md](MIGRATION.md) if coming from libzmq or zmq.rs
- See [examples/](../examples/) for complete working examples
- Review [SECURITY_GUIDE.md](SECURITY_GUIDE.md) for production deployments
- Check [INTEROP_TESTING.md](INTEROP_TESTING.md) for testing with libzmq
- Explore [API Documentation](https://docs.rs/monocoque) for full API reference

## Support

- **GitHub Issues**: https://github.com/vorjdux/monocoque/issues
- **Discussions**: https://github.com/vorjdux/monocoque/discussions
- **ZeroMQ Community**: https://zeromq.org/community/

---

**Version**: 0.1.0  
**License**: MIT OR Apache-2.0  
**Last Updated**: January 25, 2026
