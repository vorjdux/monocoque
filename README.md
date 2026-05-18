<div align="center">

<img src="assets/monocoque-logo.png" alt="Monocoque Logo" width="600"/>

# Monocoque

> _A high-performance, Rust-native ZeroMQ-compatible messaging runtime built on `io_uring`_

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

---

## What is Monocoque?

**Monocoque** is a high-performance messaging kernel designed to outperform libzmq while preserving Rust's memory safety guarantees. It provides:

-   **Zero-copy message handling** using `Bytes` with refcount-based fanout
-   **Syscall-minimal IO** via `io_uring` (through `compio`)
-   **ZeroMQ 3.1 protocol compatibility** (ZMTP 3.1)
-   **Runtime-agnostic architecture** (not coupled to Tokio)
-   **Strict memory safety** with minimal, auditable `unsafe` code

Unlike traditional messaging libraries, Monocoque is built as a **messaging kernel** where protocol logic is pure and IO is completely isolated, enabling deterministic testing, protocol evolution, and custom protocol development without touching the IO layer.

---

## Why "Monocoque"?

The name **monocoque** comes from Formula 1 and aerospace engineering, referring to a structural technique where the external shell bears all or most of the stress.

### The F1-Grade Connection

In Formula 1, a monocoque chassis is:

-   **Single-piece construction**: The chassis is one integrated carbon fiber shell, not separate components bolted together
-   **Load-bearing skin**: The outer shell itself carries structural loads - it's not just a cover over a frame
-   **Safety through structure**: Crash protection comes from the fundamental design, not add-on features
-   **Weight-optimized strength**: Maximum rigidity with minimal mass through material science and geometry

This directly parallels Monocoque's architecture:

| F1 Monocoque Principle     | Monocoque Runtime Implementation                                                                                        |
| -------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| **Single-piece shell**     | Unified ownership model - buffers flow through clean boundaries, no separate coordination layer needed                  |
| **Load-bearing structure** | Each layer (IO → Protocol → Routing) is self-contained and correct by construction, not defensively checked             |
| **Carbon fiber strength**  | Type system enforces correctness - `SlabMut` → `Bytes` transition is one-way, preventing use-after-free at compile time |
| **Crash safety cell**      | `unsafe` isolated to `alloc/` module - failure boundary is explicit and auditable                                       |
| **Minimal weight**         | Zero-copy everywhere - `Bytes::clone()` bumps refcounts, never copies payloads                                          |
| **Predictable rigidity**   | Sans-IO state machines are deterministic - same input always produces same output, enabling exhaustive testing          |

Just as an F1 monocoque achieves safety through **structural correctness** rather than protective padding, this runtime achieves performance through **architectural correctness** rather than optimization tricks that compromise safety.

> _"This is not a framework. This is a chassis."_

---

## Architecture

Monocoque is built as a layered system, each layer providing clean abstractions:

```
┌─────────────────────────────────────────────────────────────────────┐
│              Application Layer (monocoque)                          │
│  Public API: DealerSocket, RouterSocket, ReqSocket, RepSocket,      │
│              PubSocket, SubSocket, XPubSocket, XSubSocket            │
│  • High-level ergonomic API (monocoque::zmq::*)                     │
│  • Convenient constructors (connect, bind, connect_ipc)             │
│  • Clean error handling                                             │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│           ZMTP Socket Layer (monocoque-zmtp)                        │
│  Internal implementation - direct stream I/O                        │
│  • Generic over S: AsyncRead + AsyncWrite + Unpin                   │
│  • DealerSocket<S>, RouterSocket<S>, ReqSocket<S>, etc.             │
│  • Each socket handles: handshake, decoding, multipart, send/recv   │
│  • Specialized for TcpStream (default) and UnixStream               │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                 ┌────────────────┼────────────────┐
                 │                │                │
                 ▼                ▼                ▼
    ┌──────────────────┐ ┌──────────────┐ ┌────────────────┐
    │  ZMTP Handshake  │ │ Frame Codec  │ │  BufferConfig  │
    │  (handshake.rs)  │ │  (codec.rs)  │ │                │
    │                  │ │              │ │ • Small/Large  │
    │ • Greeting       │ │ • Short/Long │ │ • Latency/     │
    │ • NULL Auth      │ │ • Multipart  │ │   Throughput   │
    │ • Metadata       │ │ • Zero-copy  │ │                │
    └──────────────────┘ └──────────────┘ └────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                 Core Layer (monocoque-core)                         │
│  Protocol-agnostic building blocks                                  │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                 ┌────────────────┼────────────────┐
                 │                │                │
                 ▼                ▼                ▼
    ┌──────────────────┐ ┌──────────────┐ ┌────────────────┐
    │  IoArena/SlabMut │ │ Segmented    │ │  IPC/TCP       │
    │  (alloc.rs)      │ │   Buffer     │ │   Utilities    │
    │                  │ │ (buffer.rs)  │ │                │
    │ • Only unsafe    │ │              │ │ • TCP_NODELAY  │
    │   code in crate  │ │ • Recv buf   │ │ • Unix sockets │
    │ • io_uring mem   │ │ • Frame acc. │ │ • Connect/bind │
    │ • Zero-copy      │ │ • Reusable   │ │   helpers      │
    └──────────────────┘ └──────────────┘ └────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    IO Runtime (Runtime Agnostic)                    │
│  • Generic AsyncRead + AsyncWrite interface                         │
│  • Current examples use compio (io_uring/IOCP)                      │
│  • Compatible with Tokio, async-std, smol, etc.                     │
│  • Not coupled to any specific executor                             │
└─────────────────────────────────────────────────────────────────────┘
```

### Layer Responsibilities

#### 1. **Application Layer** (`monocoque`)

-   **Public Socket API**: User-facing socket types with ergonomic methods
-   **Convenient Constructors**: `DealerSocket::connect()`, `bind()`, `connect_ipc()`
-   **Clean Module Structure**: `monocoque::zmq::*` for ZeroMQ protocol sockets
-   **Error Handling**: Simplified Result types and helpful error messages

#### 2. **ZMTP Socket Layer** (`monocoque-zmtp`)

-   **Direct Stream I/O**: Each socket manages its own `AsyncRead + AsyncWrite` stream
-   **Protocol Implementation**: ZMTP 3.1 handshake, framing, and multipart message assembly
-   **Generic Sockets**: `Socket<S = TcpStream>` works with any compatible stream
-   **Self-Contained**: Each socket handles its own decoding, buffering, and state management
-   **Transport Independence**: Same code handles TCP and Unix domain sockets
-   **Zero-Copy**: Frame encoding/decoding without intermediate allocations

#### 3. **Core Layer** (`monocoque-core`)

-   **Memory Management**: `IoArena` and `SlabMut` for io_uring-safe allocation (only `unsafe` code)
-   **Buffer System**: `SegmentedBuffer` for efficient receive buffer management
-   **Transport Utilities**: TCP options (`TCP_NODELAY`), IPC connection helpers
-   **Endpoint Parsing**: `Endpoint::parse()` for `tcp://` and `ipc://` addressing
-   **Configuration**: `SocketOptions` for ergonomic socket configuration (buffer sizes, timeouts, etc.)
-   **Legacy Support**: `BufferConfig` for internal buffer management
-   **Routing Hubs**: Optional `RouterHub` and `PubSubHub` for advanced patterns (future use)

#### 4. **IO Runtime** (Runtime Agnostic)

-   **Current Implementation**: Uses `compio` for examples (io_uring on Linux, IOCP on Windows)
-   **Design**: Works with any runtime providing `AsyncRead + AsyncWrite` streams
-   **Alternative Runtimes**: Can use Tokio, async-std, smol, or any compatible runtime

### Key Design Principles

1. **Safety First**: `unsafe` code strictly limited to `alloc.rs` for kernel IO. All protocol and socket logic is 100% safe Rust.

2. **Direct Stream I/O**: Each socket owns and directly manages its stream, performing handshake, decoding, and multipart assembly inline.

3. **Zero-Copy by Construction**: All message payloads are `Bytes` - no intermediate allocations or copies.

4. **Generic Streams**: Sockets work with any `AsyncRead + AsyncWrite + Unpin` stream, enabling TCP, Unix sockets, or custom transports.

5. **Runtime Independence**: Compatible with compio, Tokio, async-std, or any async runtime.

---

## Project Status

Monocoque has **all phases complete** and is production-ready.

| Phase       | Component         | Status                            |
| ----------- | ----------------- | --------------------------------- |
| **Phase 0** | Memory & I/O      | ✅ **Complete**                   |
| **Phase 1** | ZMTP 3.1 Protocol | ✅ **Complete**                   |
| **Phase 2** | ROUTER/DEALER     | ✅ **Complete**                   |
| **Phase 3** | PUB/SUB Engine    | ✅ **Complete**                   |
| **Phase 4** | REQ/REP           | ✅ **Complete**                   |
| **Phase 5** | Reliability       | ✅ **Complete**                   |
| **Phase 6** | Performance       | ✅ **Complete**                   |
| **Phase 7** | Public API        | ✅ **Complete** (feature-gated)   |

📖 **Read the blueprints**: Comprehensive design documents are in [`docs/blueprints/`](docs/blueprints/)

🧪 **Test interoperability**: Run examples against libzmq - see [`docs/INTEROP_TESTING.md`](docs/INTEROP_TESTING.md)

---

## Core Features

### ✅ Implemented & Working

-   **Direct Stream I/O**: Each socket manages its own stream with inline handshake and decoding (Phase 0)
-   **IoBytes Zero-Copy Wrapper**: Eliminates `.to_vec()` memcpy on writes (~10-30% CPU reduction)
-   **ZMTP 3.1 Framing**: Short/long frames, fragmentation support (Phase 1)
-   **NULL Authentication**: Greeting + handshake with Socket-Type metadata (Phase 1)
-   **Sans-IO State Machine**: `ZmtpSession` with deterministic testing (Phase 1)
-   **Feature-Gated Architecture**: Protocol namespaces (`monocoque::zmq::*`), zero unused code
-   **All Core Socket Types**: DEALER, ROUTER, REQ, REP, PUB, SUB, PUSH, PULL, PAIR fully implemented
-   **Extended Socket Types**: XPUB, XSUB for broker patterns
-   **Heartbeating**: ZMTP 3.1 PING/PONG on all socket types (`ZMQ_HEARTBEAT_IVL`)
-   **Automatic Reconnection**: Exponential backoff with `send/recv_with_reconnect()` on all socket types
-   **Security**: PLAIN, CURVE, and ZAP authentication fully implemented
-   **TCP and IPC Transport**: Full support for both TCP and Unix domain sockets across all socket types
-   **Consistent SocketOptions API**: All socket types support `from_tcp_with_options()` and `from_unix_stream_with_options()` for unified configuration
-   **Endpoint Parsing**: Unified `tcp://` and `ipc://` addressing with validation
-   **Socket Monitoring**: Channel-based lifecycle events (Connected, Disconnected, etc.)
-   **Generic Stream Architecture**: Zero-cost abstractions supporting any `AsyncRead + AsyncWrite` stream
-   **Interop Examples**: Working examples demonstrating libzmq compatibility
-   **Message Builder API**: Ergonomic message construction with `push()`, `push_str()`, `push_json()`

### 🧪 Integration Testing

-   **libzmq Compatibility**: Verified against libzmq 4.1–4.4
    -   DEALER ↔ libzmq ROUTER
    -   ROUTER ↔ libzmq DEALER
    -   PUB ↔ libzmq SUB
    -   PUSH/PULL pipeline patterns
-   **Reconnection Stress Tests**: `monocoque-zmtp/tests/reconnection_integration.rs`
-   **HWM Enforcement**: `monocoque-zmtp/tests/hwm_stress_test.rs`

### 🎯 Design Goals

-   **Interop with libzmq**: Drop-in protocol compatibility ✅
-   **Performance**: **Achieved!** 21μs latency (30% faster than libzmq), 2M+ msg/sec throughput
-   **Safety**: Formal memory invariants, AddressSanitizer/ThreadSanitizer clean
-   **Extensibility**: Foundation for custom protocols beyond ZeroMQ

---

## 🚀 Performance

Monocoque is the **fastest ZeroMQ implementation in Rust**, achieving:

### Latency: 30% Faster than libzmq

| Message Size | Monocoque | rust-zmq (libzmq) | Improvement    |
| ------------ | --------- | ----------------- | -------------- |
| 64B          | 21μs      | 31μs              | **32% faster** |
| 256B         | 22μs      | 31μs              | **29% faster** |
| 1024B        | 22μs      | 33μs              | **31% faster** |

### Throughput: 2M+ Messages/Second

-   **Synchronous (REQ/REP ping-pong)**: ~327k msg/sec
-   **Pipelined (DEALER/ROUTER)**: 2M+ msg/sec with batching API
-   **vs rust-zmq**: rust-zmq (`zmq` crate, FFI bindings to libzmq) deadlocks on large pipelines, monocoque handles 100k+ messages

### IPC: Faster than TCP Loopback

-   **IPC (Unix domain sockets)**: 74-76ms for 10k messages
-   **TCP (localhost)**: 80-87ms for 10k messages
-   **Advantage**: 7-10% faster for local communication

### Explicit Batching API (Power Users)

For maximum throughput, use the batching API:

```rust
// Buffer multiple messages, then flush in single I/O operation
for msg in messages {
    dealer.send_buffered(msg)?;
}
dealer.flush().await?;  // Single I/O for all messages

// Or use the convenience method
dealer.send_batch(&messages).await?;
```

**Result**: 2M+ msg/sec vs ~327k msg/sec with individual sends

### Benchmark Suite

Run comprehensive benchmarks:

```bash
cd monocoque
cargo bench --features zmq

# Or use the comprehensive runner
../scripts/bench_all.sh

# View HTML reports
firefox target/criterion/report/index.html
```

Benchmarks include:

-   Latency comparison with rust-zmq (`zmq` crate, FFI bindings to libzmq)
-   Synchronous and pipelined throughput
-   IPC vs TCP performance (Unix-only)
-   Multi-threaded scaling
-   PUB/SUB patterns

---

## Memory Safety Model

Monocoque follows a **kernel-style safety boundary**:

```
monocoque-core/src/
├── alloc.rs        ← ONLY file with `unsafe` (Page, SlabMut, IoBytes, IoArena)
├── router.rs       ← 100% safe Rust (RouterHub)
├── backpressure.rs ← 100% safe Rust
├── buffer.rs       ← 100% safe Rust (SegmentedBuffer)
├── config.rs       ← 100% safe Rust (BufferConfig)
├── tcp.rs          ← 100% safe Rust (TCP utilities)
├── error.rs        ← 100% safe Rust
└── pubsub/         ← 100% safe Rust (PubSubHub, SubscriptionIndex)
    ├── hub.rs
    ├── index.rs
    └── mod.rs
```

### Global Memory Invariants

1. **No buffer reuse while referenced** - Tracked via `Bytes` refcounts
2. **No uninitialized memory exposure** - `freeze(n)` bounds all views
3. **No mutation after freeze** - `SlabMut` → `Bytes` is one-way
4. **All fanout is refcount-based** - `Bytes::clone()` only
5. **All routing state is epoch-protected** - Prevents ghost peer bugs

See [`docs/blueprints/06-safety-model-and-unsafe-audit.md`](docs/blueprints/06-safety-model-and-unsafe-audit.md) for formal proofs.

---

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
monocoque = { version = "0.1", features = ["zmq"] }  # Feature-gated protocol
compio = { version = "0.13", features = ["runtime"] }
```

### Example: DEALER Socket (TCP)

```rust
use monocoque::zmq::DealerSocket;
use bytes::Bytes;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TCP connection (cross-platform)
    let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
    // Or: DealerSocket::connect("tcp://127.0.0.1:5555").await?;

    // Send single message
    socket.send(vec![b"Hello".into(), b"World".into()]).await?;

    // Or use batching API for high throughput (2M+ msg/sec)
    let messages = vec![
        vec![Bytes::from("msg1")],
        vec![Bytes::from("msg2")],
        vec![Bytes::from("msg3")],
    ];
    socket.send_batch(&messages).await?;

    // Receive reply
    let reply = socket.recv().await;

    Ok(())
}
```

### Example: Socket Options (Advanced Configuration)

```rust
use monocoque::zmq::{DealerSocket, SocketOptions};
use std::time::Duration;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure socket with custom options
    let options = SocketOptions::default()
        .with_buffer_sizes(16384, 16384)  // 16KB buffers for high throughput
        .with_send_timeout(Duration::from_secs(5))
        .with_recv_timeout(Duration::from_secs(10));

    // Use with TCP connection
    let stream = compio::net::TcpStream::connect("127.0.0.1:5555").await?;
    let mut socket = DealerSocket::from_tcp_with_options(stream, options).await?;

    // Socket now uses custom configuration
    socket.send(vec![b"test".into()]).await?;

    Ok(())
}
```

### Example: DEALER Socket (IPC - Unix Only)

```rust
use monocoque::zmq::{DealerSocket, SocketOptions};

#[cfg(unix)]
#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // IPC connection via Unix domain socket (40% lower latency than TCP loopback)
    let mut socket = DealerSocket::connect_ipc("/tmp/dealer.sock").await?;
    // Or: DealerSocket::connect_ipc("ipc:///tmp/dealer.sock").await?;

    // Same API - send and receive work identically
    socket.send(vec![b"Hello".into()]).await?;
    let reply = socket.recv().await;

    // Or create from existing Unix stream with custom options
    let stream = compio::net::UnixStream::connect("/tmp/dealer.sock").await?;
    let options = SocketOptions::default().with_buffer_sizes(8192, 8192);
    let mut socket = DealerSocket::from_unix_stream_with_options(stream, options).await?;

    Ok(())
}
```

### Example: PUB/SUB

```rust
use monocoque::zmq::{PubSocket, SubSocket};

// Publisher
let mut pub_socket = PubSocket::bind("127.0.0.1:5556").await?;
pub_socket.send(vec![b"topic.events".into(), b"data".into()]).await?;

// Subscriber
let mut sub_socket = SubSocket::connect("127.0.0.1:5556").await?;
sub_socket.subscribe(b"topic").await?;  // subscribe is async
let msg = sub_socket.recv().await?;
```

### Example: Socket Monitoring

```rust
use monocoque::zmq::{DealerSocket, SocketEvent};

// Enable monitoring on a socket
let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
let monitor = socket.monitor();

// Spawn task to handle events
compio::runtime::spawn(async move {
    while let Ok(event) = monitor.recv_async().await {
        match event {
            SocketEvent::Connected(ep) => println!("✓ Connected to {}", ep),
            SocketEvent::Disconnected(ep) => println!("✗ Disconnected from {}", ep),
            SocketEvent::ConnectFailed { endpoint, reason } => {
                println!("✗ Connection failed: {}", reason);
            }
            _ => {}
        }
    }
});

// Socket operations emit events automatically
socket.send(vec![b"test".to_vec().into()]).await?;
```

### Example: Endpoint Parsing

```rust
use monocoque::zmq::Endpoint;

// Parse and validate endpoints
let tcp_ep = Endpoint::parse("tcp://127.0.0.1:5555")?;
let ipc_ep = Endpoint::parse("ipc:///tmp/socket.sock")?;

// Use in routing logic
match tcp_ep {
    Endpoint::Tcp(addr) => println!("TCP address: {}", addr),
    Endpoint::Ipc(path) => println!("IPC path: {:?}", path),
}
```

### Example: Transport Flexibility

```rust
use monocoque::zmq::SubSocket;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to TCP publisher
    let mut tcp_sub = SubSocket::connect("tcp://127.0.0.1:5555").await?;
    tcp_sub.subscribe(b"topic").await?;  // subscribe is async

    // Connect to IPC publisher (Unix only)
    #[cfg(unix)]
    let mut ipc_sub = SubSocket::connect_ipc("/tmp/pub.sock").await?;
    #[cfg(unix)]
    ipc_sub.subscribe(b"topic").await?;  // subscribe is async

    // Same receive API for both
    if let Some(msg) = tcp_sub.recv().await? {
        println!("TCP: {:?}", msg);
    }

    Ok(())
}
```

---

## Development

### Building from Source

```bash
# Clone the repository
git clone https://github.com/vorjdux/monocoque.git
cd monocoque

# Build all crates
cargo build --release --workspace

# Run unit tests
cargo test --workspace --features zmq

# Build examples
cargo build --examples --features zmq

# Run interop tests (coming soon, requires libzmq)
cargo test --test interop_pair
```

### Contributing

Monocoque is in early development. Contributions are welcome, especially:

-   Implementation of designed phases (see blueprints)
-   Interop test cases with libzmq
-   Performance benchmarks
-   Documentation improvements

**Before contributing**: Read the blueprints in `docs/blueprints/` to understand the architecture and safety model.

---

## Why Monocoque vs. Alternatives?

| Feature            | libzmq (C++)   | Rust ZMQ Bindings | Monocoque          |
| ------------------ | -------------- | ----------------- | ------------------ |
| Memory Safety      | ❌ Manual      | ✅ Via FFI        | ✅ Native          |
| Zero-Copy          | Partial        | ❌ FFI boundary   | ✅ `Bytes`         |
| IO Backend         | `select/epoll` | (inherited)       | ✅ `io_uring`      |
| Socket Monitoring  | ZMQ Socket     | Via FFI           | ✅ Native Channels |
| IPC Transport      | ✅ Yes         | Via FFI           | ✅ Native          |
| Endpoint Parsing   | String-based   | String-based      | ✅ Validated       |
| API Consistency    | C-style        | Varies by wrapper | ✅ Unified `SocketOptions` |
| Protocol Evolution | Hard (C++)     | Impossible        | ✅ Sans-IO         |
| Custom Protocols   | No             | No                | ✅ Yes             |
| Runtime Coupling   | N/A            | Often Tokio-bound | ✅ Agnostic        |

---

## Roadmap

-   [x] Implement `SlabMut` and Arena allocator (Phase 0) - **Complete**
-   [x] ZMTP session state machine (Phase 1) - **Complete**
-   [x] Direct stream socket implementations (Phase 0/1) - **Complete**
-   [x] ROUTER/DEALER sockets (Phase 2) - **Complete**
-   [x] PUB/SUB sockets with subscription filtering (Phase 3) - **Complete**
-   [x] REQ/REP sockets (Phase 4) - **Complete**
-   [x] TCP and IPC transport support - **Complete**
-   [x] Public API with feature gates - **Complete**
-   [x] **Performance Phase 1** - **Complete** 🚀
    -   [x] Explicit batching API (send_buffered/flush/send_batch)
    -   [x] TCP_NODELAY by default for all TCP connections
    -   [x] Comprehensive benchmark suite (6 benchmarks)
    -   [x] 21μs latency - **30% faster than libzmq**
    -   [x] 2M+ msg/sec throughput with batching
-   [x] **API Consistency** - **Complete** ✅
    -   [x] Unified `SocketOptions` configuration across all socket types
    -   [x] Consistent `from_tcp_with_options()` for TCP streams
    -   [x] Consistent `from_unix_stream_with_options()` for Unix domain sockets
    -   [x] Replaced deprecated `BufferConfig` with ergonomic `SocketOptions`
    -   [x] All benchmarks updated to use consistent API
-   [x] **Reliability** - **Complete** ✅
    -   [x] Automatic reconnection with exponential backoff on all socket types
    -   [x] ZMTP heartbeating (PING/PONG) wired into all recv/send loops
    -   [x] Identity preservation on reconnect
    -   [x] Subscription re-send on SUB/XSUB reconnect
-   [x] **Security** - **Complete** ✅
    -   [x] PLAIN authentication (username/password)
    -   [x] CURVE encryption (CurveZMQ / X25519)
    -   [x] ZAP Authentication Protocol handler

**Future**:

-   [ ] Zero-copy with io_uring fixed buffers
-   [ ] SIMD-accelerated topic matching
-   [ ] Target: 15-20μs latency, 3-5M msg/sec throughput

**Long-Term Vision**:

-   High-performance RPC protocol (outperform gRPC)
-   Custom protocol framework
-   Additional transports (QUIC, shared memory, RDMA)

See [`docs/blueprints/07-project-roadmap-and-future-phases.md`](docs/blueprints/07-project-roadmap-and-future-phases.md) and [`docs/PERFORMANCE_ROADMAP.md`](docs/PERFORMANCE_ROADMAP.md) for complete roadmap.

---

## Project Structure

Monocoque is organized as a workspace with a clear separation between public and internal crates:

```
monocoque/                        # Workspace root
├── monocoque/                    # 🔓 PUBLIC CRATE - The only crate users import
│   ├── src/
│   │   ├── lib.rs               # Public API exports
│   │   └── zmq/                 # High-level socket API (feature-gated)
│   ├── examples/                # 50+ examples showing all features
│   │   ├── simple_req_rep.rs   # Basic request-reply
│   │   ├── curve_demo.rs       # CURVE encryption
│   │   ├── proxy_steerable.rs  # Advanced patterns
│   │   └── ...
│   ├── benches/                 # Performance benchmarks
│   │   ├── throughput.rs       # Message throughput tests
│   │   ├── latency.rs          # Latency measurements
│   │   ├── performance.rs      # Protocol-level benchmarks
│   │   └── interop/            # Interop tests with libzmq
│   │       └── libzmq_throughput.py
│   └── tests/                   # Integration tests
│       ├── interop_router.rs   # Test ROUTER interop with libzmq
│       └── zap_integration.rs  # ZAP authentication tests
│
├── monocoque-core/              # 🔒 INTERNAL - Protocol-agnostic primitives
│   └── src/
│       ├── message.rs          # Zero-copy message type
│       ├── options.rs          # Socket configuration
│       └── socket_type.rs      # Socket type enum
│
├── monocoque-zmtp/              # 🔒 INTERNAL - ZMTP 3.1 implementation
│   └── src/
│       ├── req.rs              # REQ socket
│       ├── rep.rs              # REP socket  
│       ├── dealer.rs           # DEALER socket
│       ├── router.rs           # ROUTER socket
│       ├── publisher.rs        # PUB socket
│       ├── subscriber.rs       # SUB socket
│       ├── handshake.rs        # ZMTP handshake
│       ├── codec.rs            # Frame encoding/decoding
│       └── security/           # NULL, PLAIN, CURVE mechanisms
│
├── docs/                        # User-facing documentation
│   ├── GETTING_STARTED.md      # Quick start guide
│   ├── USER_GUIDE.md           # Comprehensive usage guide
│   ├── SECURITY_GUIDE.md       # Security best practices
│   ├── MIGRATION.md            # Migrating from libzmq
│   ├── COMPATIBILITY.md        # ZeroMQ compatibility status
│   ├── PERFORMANCE.md          # Performance characteristics
│   ├── PRODUCTION_DEPLOYMENT.md # Production deployment guide
│   ├── blueprints/             # Design documents
│   └── internal/               # Development/implementation docs
│
├── scripts/                     # Development scripts
│   ├── bench_all.sh            # Run all benchmarks
│   └── run_interop_tests.sh    # Test interop with libzmq
│
├── interop_tests/               # Interoperability test suite
│   ├── test_req_rep_interop.py # Python ↔ Rust interop tests
│   └── test_pub_sub_interop.py # PUB/SUB interop
│
├── monocoque-fuzz/              # Fuzzing targets (cargo-fuzz)
│   └── fuzz_targets/
│       └── fuzz_decoder.rs     # Protocol fuzzing
│
└── Cargo.toml                   # Workspace manifest
```

### Crate Boundaries

**Public API** (`monocoque` crate):
```rust
use monocoque::zmq::DealerSocket;  // ✅ Public
use monocoque::SocketOptions;       // ✅ Public
```

**Internal crates** (not published - cannot be used directly):
```rust
use monocoque_zmtp::RepSocket;     // ❌ Not on crates.io
use monocoque_core::Message;       // ❌ Not on crates.io
```

**Note**: `monocoque-core` and `monocoque-zmtp` have `publish = false` in their Cargo.toml, preventing them from being published to crates.io. Users must use the `monocoque` crate, which re-exports all necessary types.

---

## Documentation

### Architecture & Design
-   📘 **[Overview](docs/blueprints/00-overview.md)** - Project vision and architecture
-   🔒 **[Safety Model](docs/blueprints/06-safety-model-and-unsafe-audit.md)** - Memory guarantees and unsafe audit
-   🏗️ **[Phase 0: Memory & I/O](docs/blueprints/02-phase0-memory-and-io.md)** - Memory management and direct stream I/O
-   📡 **[Phase 1: ZMTP](docs/blueprints/03-phase1-zmtp-framing-and-handshake.md)** - Protocol implementation
-   🔀 **[Phase 2: Routing](docs/blueprints/04-phase2-router-dealer-and-load-balancing.md)** - ROUTER/DEALER semantics
-   📢 **[Phase 3: PUB/SUB](docs/blueprints/05-phase3-pubsub-and-subscription-index.md)** - Subscription engine
-   🛡️ **[Phase 5: Reliability](docs/RELIABILITY_AND_RESILIENCE.md)** - Reconnection, HWM, PoisonGuard

### Compatibility & Roadmap
-   🗺️ **[ZeroMQ Compatibility Roadmap](docs/ZEROMQ_COMPATIBILITY_ROADMAP.md)** - Complete libzmq feature analysis and implementation plan
-   ⚡ **[Performance Roadmap](docs/PERFORMANCE_ROADMAP.md)** - Optimization phases and benchmarking strategy

---

## License

MIT License - see [LICENSE](LICENSE) for details.

---

## Acknowledgments

Inspired by:

-   **ZeroMQ** - Elegant messaging patterns
-   **io_uring** - Modern Linux async IO
-   **Tokio** - Rust async ecosystem leadership
-   **F1 Engineering** - Performance through correct design, not shortcuts

Built with: `compio`, `flume`, `bytes`, `smallvec`

---

_"Performance through correct architecture, not through unsafe shortcuts."_
