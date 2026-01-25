# Monocoque User Guide

**Complete guide to building ZeroMQ applications with monocoque**

**Version**: 0.1.0  
**Last Updated**: January 25, 2026

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
use monocoque_zmtp::RepSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    // Bind and listen for requests
    let mut server = RepSocket::from_tcp("tcp://127.0.0.1:5555").await?;
    
    loop {
        // Receive request
        let request = server.recv().await?.expect("Connection closed");
        println!("Received: {:?}", request);
        
        // Send reply
        let reply = vec![Bytes::from("World")];
        server.send(reply).await?;
    }
}
```

**Client** (REQ socket):
```rust
use monocoque_zmtp::ReqSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> std::io::Result<()> {
    // Connect to server
    let mut client = ReqSocket::from_tcp("tcp://127.0.0.1:5555").await?;
    
    // Send request
    let request = vec![Bytes::from("Hello")];
    client.send(request).await?;
    
    // Receive reply
    let reply = client.recv().await?.expect("No reply");
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
use monocoque_core::options::SocketOptions;
use std::time::Duration;

let options = SocketOptions::new()
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_timeout(Duration::from_secs(5))
    .with_recv_hwm(1000)
    .with_send_hwm(1000)
    .with_immediate(true);

let socket = DealerSocket::with_options(options);
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
let mut client = ReqSocket::from_tcp("tcp://server:5555").await?;
client.send(vec![Bytes::from("ping")]).await?;
let response = client.recv().await?;

// Server
let mut server = RepSocket::from_tcp("tcp://*:5555").await?;
let request = server.recv().await?;
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
let mut client = DealerSocket::from_tcp("tcp://server:5555").await?;
for i in 0..10 {
    client.send(vec![Bytes::from(format!("Request {}", i))]).await?;
}

// Server (ROUTER)
let mut server = RouterSocket::from_tcp("tcp://*:5555").await?;
loop {
    let msg = server.recv().await?;
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
let mut publisher = PubSocket::from_tcp("tcp://*:5556").await?;
loop {
    let event = vec![
        Bytes::from("weather"),              // Topic
        Bytes::from("temperature: 72°F"),    // Data
    ];
    publisher.send(event).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
}

// Subscriber
let mut subscriber = SubSocket::from_tcp("tcp://server:5556").await?;
subscriber.subscribe(b"weather").await?;
while let Some(msg) = subscriber.recv().await? {
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
// Broker (proxy pattern)
let mut frontend = XSubSocket::from_tcp("tcp://*:5559").await?;
let mut backend = XPubSocket::from_tcp("tcp://*:5560").await?;

monocoque_zmtp::proxy::proxy(&mut frontend, &mut backend, None).await?;
```

### 3. Pipeline Pattern

#### PUSH/PULL

**Use When:**
- Distributing tasks to workers
- Parallel processing
- One-way data flow

**Example:**
```rust
// Ventilator (task producer)
let mut ventilator = PushSocket::from_tcp("tcp://*:5557").await?;
for i in 0..100 {
    ventilator.send(vec![Bytes::from(format!("Task {}", i))]).await?;
}

// Worker
let mut receiver = PullSocket::from_tcp("tcp://ventilator:5557").await?;
let mut sender = PushSocket::from_tcp("tcp://sink:5558").await?;
while let Some(task) = receiver.recv().await? {
    // Process task
    let result = process(task);
    sender.send(result).await?;
}

// Sink (result collector)
let mut sink = PullSocket::from_tcp("tcp://*:5558").await?;
for _ in 0..100 {
    let result = sink.recv().await?;
    println!("Result: {:?}", result);
}
```

---

## Advanced Features

### Security

#### PLAIN Authentication

Username/password authentication:

```rust
use monocoque_core::options::SocketOptions;

// Server
let options = SocketOptions::new().with_plain_server(true);
let server = RepSocket::with_options(options);

// Client
let options = SocketOptions::new()
    .with_plain_credentials("admin", "secret123");
let client = ReqSocket::with_options(options);
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
use monocoque_zmtp::proxy::proxy;

let mut frontend = RouterSocket::from_tcp("tcp://*:5559").await?;
let mut backend = DealerSocket::from_tcp("tcp://*:5560").await?;

// Bidirectional forwarding
proxy(&mut frontend, &mut backend, None).await?;
```

Steerable proxy with control socket:

```rust
use monocoque_zmtp::proxy::proxy_steerable;

let mut control = PairSocket::from_tcp("tcp://127.0.0.1:5561").await?;

proxy_steerable(&mut frontend, &mut backend, None, &mut control).await?;

// From another thread:
// Send b"PAUSE", b"RESUME", b"TERMINATE", or b"STATISTICS"
```

---

## Best Practices

### 1. Error Handling

Always handle potential errors:

```rust
// Good: Handle connection failures
match DealerSocket::from_tcp("tcp://server:5555").await {
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
let mut socket = ReqSocket::with_options(options);

match socket.recv().await {
    Ok(Some(msg)) => println!("Received: {:?}", msg),
    Ok(None) => println!("Timeout"),
    Err(e) => eprintln!("Error: {}", e),
}
```

### 2. Resource Management

Use RAII for automatic cleanup:

```rust
{
    let socket = DealerSocket::from_tcp("tcp://server:5555").await?;
    // Use socket
} // Socket automatically closed here
```

Set linger for graceful shutdown:

```rust
let options = SocketOptions::new()
    .with_linger(Duration::from_millis(100));
```

### 3. Message Construction

Use the message builder for clarity:

```rust
use monocoque_core::message_builder::Message;

let msg = Message::new()
    .push_str("topic")
    .push_empty()        // Delimiter
    .push_str("payload")
    .into_frames();

socket.send(msg).await?;
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
    .with_read_buffer_size(16 * 1024)   // 16KB read buffer
    .with_write_buffer_size(16 * 1024); // 16KB write buffer
```

Presets for common cases:

```rust
let options = SocketOptions::small();  // 4KB buffers
let options = SocketOptions::large();  // 16KB buffers
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
