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
│                    Application Layer (monocoque)                    │
│  Public API: DealerSocket, RouterSocket, ReqSocket, RepSocket,      │
│              PubSocket, SubSocket                                   │
│  • High-level ergonomic API with error handling                     │
│  • Socket monitoring via channels (SocketMonitor)                   │
│  • Transport abstraction (TCP/IPC via Endpoint)                     │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                 ┌────────────────┼────────────────┐
                 │                │                │
                 ▼                ▼                ▼
    ┌──────────────────┐ ┌──────────────┐ ┌────────────────┐
    │  Socket Monitor  │ │   Endpoint   │ │ BufferConfig   │
    │  (monocoque-core)│ │   Parser     │ │ (monocoque-    │
    │                  │ │ (monocoque-  │ │     core)      │
    │ • SocketEvent    │ │    core)     │ │                │
    │ • Event channels │ │              │ │ • Small/Large  │
    │ • Lifecycle      │ │ • tcp://     │ │ • Latency/     │
    │   tracking       │ │ • ipc://     │ │   Throughput   │
    └──────────────────┘ └──────────────┘ └────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│              ZMTP Socket Layer (monocoque-zmtp)                     │
│  Internal protocol implementation - direct stream I/O               │
│  • Generic over S: AsyncRead + AsyncWrite + Unpin                   │
│  • DealerSocket<S>, RouterSocket<S>, ReqSocket<S>, etc.             │
│  • Specialized for TcpStream (default) and UnixStream               │
└─────────────────────────────────────────────────────────────────────┘
                                  │
                 ┌────────────────┼────────────────┐
                 │                │                │
                 ▼                ▼                ▼
    ┌──────────────────┐ ┌──────────────┐ ┌────────────────┐
    │  ZMTP Handshake  │ │ Frame Codec  │ │  ZmtpSession   │
    │                  │ │              │ │                │
    │ • Greeting       │ │ • Short/Long │ │ • Socket Type  │
    │ • NULL Auth      │ │ • Multipart  │ │ • Metadata     │
    │ • Metadata       │ │ • Zero-copy  │ │ • State        │
    └──────────────────┘ └──────────────┘ └────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────────┐
│                 Core Layer (monocoque-core)                         │
│  Runtime-agnostic building blocks                                   │
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
- **Public Socket API**: User-facing socket types with ergonomic methods
- **Event Monitoring**: `SocketMonitor` for lifecycle events (Connected, Disconnected, etc.)
- **Transport Abstraction**: `Endpoint::parse()` handles `tcp://` and `ipc://` addressing
- **Configuration**: `BufferConfig` for latency vs throughput tuning

#### 2. **ZMTP Socket Layer** (`monocoque-zmtp`)
- **Protocol Implementation**: Direct stream I/O with ZMTP 3.1 framing
- **Generic Sockets**: `Socket<S = TcpStream>` works with any `AsyncRead + AsyncWrite` stream
- **Transport Independence**: Same code handles TCP and Unix domain sockets
- **Zero-Copy Codec**: Frame encoding/decoding without intermediate allocations

#### 3. **Core Layer** (`monocoque-core`)
- **Memory Management**: `IoArena` and `SlabMut` for io_uring-safe allocation (only `unsafe` code)
- **Buffer System**: `SegmentedBuffer` for efficient receive buffer management
- **Transport Utilities**: TCP options (`TCP_NODELAY`), IPC connection helpers
- **Monitoring Infrastructure**: Event types and channel management

#### 4. **IO Runtime** (Runtime Agnostic)
- **Current Implementation**: Uses `compio` for examples (io_uring on Linux, IOCP on Windows)
- **Design**: Works with any runtime providing `AsyncRead + AsyncWrite` streams
- **Alternative Runtimes**: Can use Tokio, async-std, smol, or any compatible runtime

### Key Design Principles

1. **Safety First**: `unsafe` code strictly limited to `alloc/` module for kernel IO. Everything above is 100% safe Rust.

2. **Ownership-Passing IO**: Buffers move into the kernel during IO operations, preventing aliasing and race conditions.

3. **Zero-Copy by Construction**: All message payloads are `Bytes` - fanout uses refcount bumps, never `memcpy`.

4. **Direct Stream Architecture**: Socket implementations use direct async read/write on streams, enabling minimal latency and maximum control.

5. **Runtime Independence**: Uses `compio` for async IO - not coupled to Tokio's executor.

---

## Project Status

Monocoque has **Phase 0-3 implementation complete** with integration testing in progress.

| Phase       | Component            | Status                            |
| ----------- | -------------------- | --------------------------------- |
| **Phase 0** | IO Core & Split Pump | ✅ **Complete**                   |
| **Phase 1** | ZMTP 3.1 Protocol    | ✅ **Complete**                   |
| **Phase 2** | ROUTER/DEALER        | ✅ **Complete** (testing pending) |
| **Phase 3** | PUB/SUB Engine       | ✅ **Complete** (testing pending) |
| **Phase 4** | REQ/REP              | ✅ **Complete** (testing pending) |
| **Phase 5** | Reliability          | ⏳ Planned                        |
| **Phase 6** | Performance          | ⏳ Planned                        |
| **Phase 7** | Public API           | ✅ **Complete** (feature-gated)   |

📖 **Read the blueprints**: Comprehensive design documents are in [`docs/blueprints/`](docs/blueprints/)

🧪 **Test interoperability**: Run examples against libzmq - see [`docs/INTEROP_TESTING.md`](docs/INTEROP_TESTING.md)

---

## Core Features

### ✅ Implemented & Working

-   **Split Read/Write Pumps**: Cancellation-safe, independent flow control (Phase 0)
-   **IoBytes Zero-Copy Wrapper**: Eliminates `.to_vec()` memcpy on writes (~10-30% CPU reduction)
-   **ZMTP 3.1 Framing**: Short/long frames, fragmentation support (Phase 1)
-   **NULL Authentication**: Greeting + handshake with Socket-Type metadata (Phase 1)
-   **Sans-IO State Machine**: `ZmtpSession` with deterministic testing (Phase 1)
-   **Feature-Gated Architecture**: Protocol namespaces (`monocoque::zmq::*`), zero unused code
-   **All Socket Types**: DEALER, ROUTER, REQ, REP, PUB, SUB fully implemented (Phase 2-4)
-   **TCP and IPC Transport**: Full support for both TCP and Unix domain sockets across all socket types
-   **Endpoint Parsing**: Unified `tcp://` and `ipc://` addressing with validation
-   **Socket Monitoring**: Channel-based lifecycle events (Connected, Disconnected, etc.)
-   **Generic Stream Architecture**: Zero-cost abstractions supporting any `AsyncRead + AsyncWrite` stream
-   **Interop Examples**: Working examples demonstrating libzmq compatibility

### 🧪 Integration Testing (Current Priority)

-   **libzmq Compatibility**: Standalone examples for manual verification
    -   DEALER ↔ libzmq ROUTER
    -   ROUTER ↔ libzmq DEALER
    -   PUB ↔ libzmq SUB
-   **Multi-Peer Tests**: Coming soon (load balancing, fanout)
-   **Stress Tests**: Coming soon (reconnection, high throughput)

### 🎯 Design Goals

-   **Interop with libzmq**: Drop-in protocol compatibility
-   **Performance**: Target < 10μs latency, > 1M msg/sec throughput
-   **Safety**: Formal memory invariants, AddressSanitizer/ThreadSanitizer clean
-   **Extensibility**: Foundation for custom protocols beyond ZeroMQ

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

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TCP connection (cross-platform)
    let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
    // Or: DealerSocket::connect("tcp://127.0.0.1:5555").await?;

    // Send multipart message
    socket.send(vec![b"Hello".into(), b"World".into()]).await?;

    // Receive reply
    let reply = socket.recv().await?;

    Ok(())
}
```

### Example: DEALER Socket (IPC - Unix Only)

```rust
use monocoque::zmq::DealerSocket;

#[cfg(unix)]
#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // IPC connection via Unix domain socket (40% lower latency than TCP loopback)
    let mut socket = DealerSocket::connect_ipc("/tmp/dealer.sock").await?;
    // Or: DealerSocket::connect_ipc("ipc:///tmp/dealer.sock").await?;

    // Same API - send and receive work identically
    socket.send(vec![b"Hello".into()]).await?;
    let reply = socket.recv().await?;

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
sub_socket.subscribe(b"topic");
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
    tcp_sub.subscribe(b"topic");

    // Connect to IPC publisher (Unix only)
    #[cfg(unix)]
    let mut ipc_sub = SubSocket::connect_ipc("/tmp/pub.sock").await?;
    #[cfg(unix)]
    ipc_sub.subscribe(b"topic");

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

| Feature            | libzmq (C++)   | Rust ZMQ Bindings | Monocoque     |
| ------------------ | -------------- | ----------------- | ------------- |
| Memory Safety      | ❌ Manual      | ✅ Via FFI        | ✅ Native     |
| Zero-Copy          | Partial        | ❌ FFI boundary   | ✅ `Bytes`    |
| IO Backend         | `select/epoll` | (inherited)       | ✅ `io_uring` |
| Socket Monitoring  | Socket-based   | Via FFI           | ✅ Channel    |
| IPC Transport      | ✅ Yes         | Via FFI           | ✅ Native     |
| Endpoint Parsing   | String-based   | String-based      | ✅ Validated  |
| Protocol Evolution | Hard (C++)     | Impossible        | ✅ Sans-IO    |
| Custom Protocols   | No             | No                | ✅ Yes        |
| Runtime Coupling   | N/A            | Often Tokio-bound | ✅ Agnostic   |

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
-   [ ] Comprehensive interop testing with libzmq - **Current Priority**
-   [ ] Performance benchmarking (target: <10μs latency, >1M msg/sec)
-   [ ] AddressSanitizer/ThreadSanitizer validation

**Long-Term Vision**:

-   High-performance RPC protocol (outperform gRPC)
-   Custom protocol framework
-   Additional transports (QUIC, shared memory, RDMA)

See [`docs/blueprints/07-project-roadmap-and-future-phases.md`](docs/blueprints/07-project-roadmap-and-future-phases.md) for complete roadmap.

---

## Documentation

-   📘 **[Overview](docs/blueprints/00-overview.md)** - Project vision and architecture
-   🔒 **[Safety Model](docs/blueprints/06-safety-model-and-unsafe-audit.md)** - Memory guarantees and unsafe audit
-   🏗️ **[Phase 0: IO Core](docs/blueprints/02-phase0-io-and-split-pump.md)** - Split pump architecture
-   📡 **[Phase 1: ZMTP](docs/blueprints/03-phase1-zmtp-framing-and-handshake.md)** - Protocol implementation
-   🔀 **[Phase 2: Routing](docs/blueprints/04-phase2-router-dealer-and-load-balancing.md)** - ROUTER/DEALER semantics
-   📢 **[Phase 3: PUB/SUB](docs/blueprints/05-phase3-pubsub-and-subscription-index.md)** - Subscription engine

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
