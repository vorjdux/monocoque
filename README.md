# Monocoque

> _A high-performance, Rust-native ZeroMQ-compatible messaging runtime built on `io_uring`_

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

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

Monocoque is built in phases, each providing a stable foundation for the next:

```
┌──────────────────────────────────────────┐
│         Application Layer                │
│     (UserCmd / Vec<Bytes> messages)      │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│          Routing Hubs                    │
│  RouterHub | PubSubHub | DealerLB        │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│         SocketActor                      │
│  • Read Pump (kernel → user)             │
│  • Write Pump (user → kernel)            │
│  • Multipart Bridge                      │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│      ZMTP 3.1 Session Layer              │
│  • Sans-IO State Machine                 │
│  • Framing & Handshake                   │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│     IO Arena / Slab (unsafe)             │
│     io_uring via compio                  │
└──────────────────────────────────────────┘
```

### Key Design Principles

1. **Safety First**: `unsafe` code strictly limited to `alloc/` module for kernel IO. Everything above is 100% safe Rust.

2. **Ownership-Passing IO**: Buffers move into the kernel during IO operations, preventing aliasing and race conditions.

3. **Zero-Copy by Construction**: All message payloads are `Bytes` - fanout uses refcount bumps, never `memcpy`.

4. **Sans-IO Protocol Layer**: ZMTP session logic is pure state machines (`Bytes in → Events out`), enabling deterministic testing and protocol evolution.

5. **Runtime Independence**: Uses `flume` for channels and `compio` for IO - not coupled to Tokio's executor.

---

## Project Status

Monocoque is currently in **design and early implementation phase**. The architecture is fully specified in comprehensive blueprints:

| Phase       | Component            | Status         |
| ----------- | -------------------- | -------------- |
| **Phase 0** | IO Core & Split Pump | ✅ Designed    |
| **Phase 1** | ZMTP 3.1 Protocol    | ✅ Designed    |
| **Phase 2** | ROUTER/DEALER        | ✅ Designed    |
| **Phase 3** | PUB/SUB Engine       | 🚀 In Progress |
| **Phase 4** | REQ/REP              | ⏳ Planned     |
| **Phase 5** | Reliability          | ⏳ Planned     |
| **Phase 6** | Performance          | ⏳ Planned     |
| **Phase 7** | Public API           | ⏳ Planned     |

📖 **Read the blueprints**: Comprehensive design documents are in [`docs/blueprints/`](docs/blueprints/)

---

## Core Features

### ✅ Designed & Documented

-   **Split Read/Write Pumps**: Cancellation-safe, independent flow control
-   **Vectored IO with Partial Write Handling**: Correct syscall batching
-   **ZMTP 3.1 Framing**: Short/long frames, zero-copy fast path, fragmented-frame fallback
-   **NULL Authentication**: Greeting + handshake with libzmq interop
-   **DEALER/ROUTER Semantics**: Identity envelopes, multipart messages, load balancing
-   **Epoch-Based Lifecycle**: Ghost peer prevention on reconnect
-   **Sorted Prefix Table for PUB/SUB**: Cache-friendly linear matching (not trie-based)

### 🎯 Design Goals

-   **Interop with libzmq**: Drop-in protocol compatibility
-   **Performance**: Target < 10μs latency, > 1M msg/sec throughput
-   **Safety**: Formal memory invariants, AddressSanitizer/ThreadSanitizer clean
-   **Extensibility**: Foundation for custom protocols beyond ZeroMQ

---

## Memory Safety Model

Monocoque follows a **kernel-style safety boundary**:

```
monocoque-core/
├── alloc/          ← ONLY module with `unsafe`
│   ├── slab.rs     ← SlabMut: stable IO buffers
│   ├── arena.rs    ← Arena allocator
│   └── invariants.md
├── actor/          ← 100% safe Rust
├── router/         ← 100% safe Rust
├── pubsub/         ← 100% safe Rust
└── zmtp/           ← 100% safe Rust
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

_(Coming soon - project is in design/implementation phase)_

```rust
// Future API preview (subject to change)
use monocoque::prelude::*;

let ctx = Context::new();
let socket = ctx.socket(SocketType::Router)?;
socket.bind("tcp://127.0.0.1:5555").await?;

loop {
    let msg = socket.recv().await?;
    socket.send(msg).await?;
}
```

---

## Development

### Building from Source

```bash
# Clone the repository
git clone https://github.com/vorjdux/monocoque.git
cd monocoque

# Build (when Cargo.toml is available)
cargo build --release

# Run tests
cargo test

# Run interop tests (requires libzmq)
cargo test --test libzmq_interop
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
| Protocol Evolution | Hard (C++)     | Impossible        | ✅ Sans-IO    |
| Custom Protocols   | No             | No                | ✅ Yes        |
| Runtime Coupling   | N/A            | Often Tokio-bound | ✅ Agnostic   |

---

## Roadmap

-   [ ] Implement `SlabMut` and Arena allocator (Phase 0)
-   [ ] ZMTP session state machine (Phase 1)
-   [ ] SocketActor with split pumps (Phase 0/1)
-   [ ] ROUTER/DEALER hubs (Phase 2)
-   [ ] PubSubHub with SubscriptionIndex (Phase 3)
-   [ ] Comprehensive interop testing with libzmq

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
