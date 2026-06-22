# Monocoque Development Context

## Project Overview

Monocoque is a high-performance, Rust-native ZeroMQ-compatible messaging runtime built on `io_uring` (via `compio`).

Blueprints covering design decisions live in `docs/blueprints/`.

## Core Architecture

Start here:

- `docs/blueprints/00-overview.md` - System architecture and phases
- `docs/blueprints/01-unsafe-boundary-and-allocator.md` - Safety model
- `docs/blueprints/06-safety-model-and-unsafe-audit.md` - Memory guarantees

This is a layered messaging **kernel**, not a framework. Protocol logic is pure; IO is isolated.

## Critical Safety Rules

### Unsafe Code Boundary

- `unsafe` is ONLY allowed in: `monocoque-core/src/alloc.rs` (single file containing all allocation logic)
- Everything above Phase 0 (protocol, routing, pubsub) must be 100% safe Rust
- Every `unsafe` block requires documented invariants (see blueprint 01)

### Memory Invariants

1. No buffer reuse while referenced
2. No uninitialized memory exposure
3. No mutation after freeze (`SlabMut` to `Bytes`)
4. All fanout is refcount-based (via `Bytes::clone()`)
5. All routing state is epoch-protected (ghost peer prevention)

See blueprint 06 for formal proofs.

## Implementation Phases

### Phase 0 - IO Core (complete)

- `SlabMut` with `IoBufMut` trait implementation
- Arena allocator with refcounting
- Split read/write pumps in `SocketActor`
- Partial write handling for vectored IO

Zero-copy writes use the compio `bytes` feature so `Bytes` implements `IoBuf` directly - no wrapper type needed.

```rust
// Read pump pattern
let slab = arena.alloc();
let (res, slab) = reader.read(slab).await;  // kernel owns buffer
let bytes = slab.freeze(n);                  // convert to immutable

// Write pump - Bytes implements IoBuf directly
stream.write_all(bytes).await;
```

### Phase 1 - ZMTP Protocol (complete)

- Sans-IO `ZmtpSession` state machine (Greeting to Handshake to Active)
- Frame encoder/decoder with fragmentation support
- NULL mechanism handshake
- READY command with Socket-Type metadata
- Identity ownership via `Bytes::copy_from_slice`

Note: READY message must include `Socket-Type` metadata or libzmq silently drops the peer.

### Phase 2 - Routing (complete)

- `ZmtpIntegratedActor` composing SocketActor + Session + Hubs
- DEALER socket with multipart bridge
- ROUTER socket with identity envelopes
- `RouterHub` with round-robin load balancing
- Epoch-based ghost peer prevention

Three-layer separation: `SocketActor` (IO) - `Hub` (routing) - `User API`.

### Phase 3 - PUB/SUB (complete)

- `SubscriptionIndex` with sorted prefix table
- PUB socket (broadcast send-only)
- SUB socket (subscribe/unsubscribe/recv)
- `PubSubHub` with epoch tracking
- Zero-copy fanout (Vec clone, Bytes refcount)

Linear scan with early exit for subscription matching - cache-friendly, no per-message allocation.

### Public API Layer (complete)

Crate: `monocoque-rs` (ergonomic facade)

- Feature-gated protocols: `monocoque-rs = { version = "0.1", features = ["zmq"] }`
- Zero default features (explicit opt-in)
- Idiomatic async/await API
- Protocol namespace: `monocoque::zmq::{DealerSocket, RouterSocket, PubSocket, SubSocket}`

```rust
use monocoque::zmq::DealerSocket;

let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
socket.send(vec![b"Hello".into()]).await?;
let reply = socket.recv().await?;
```

## Development Workflows

### Testing Strategy

1. **Unit tests**: Deterministic, safe Rust logic only
2. **Interop tests**: Against real `libzmq` peers (validates protocol correctness)
3. **Stress tests**: Reconnection churn, fanout, race conditions
4. **Sanitizers**: AddressSanitizer, ThreadSanitizer

Run tests: `cargo test --workspace --features zmq`

### Build Conventions

- Use `flume` for channels (runtime-agnostic, not Tokio-bound)
- Use `compio` for IO (io_uring/IOCP abstraction)
- Use `bytes` crate for zero-copy message handling
- No `tokio::select!`, no shared mutable state, no `Arc<Mutex<T>>` in hot paths

## Project-Specific Patterns

### Epoch-Based Lifecycle

```rust
// Prevent ghost peer races on reconnect
struct PeerState { epoch: u64, tx: Sender<PeerCmd> }
// PeerUp(epoch) replaces old state
// PeerDown(epoch) ignored if stale
```

Used in: ROUTER hub (Phase 2), PUB/SUB subscriptions (Phase 3).

### Zero-Copy Fanout

```rust
// PUB/SUB broadcast - clone Vec, NOT payloads
tx.send(PeerCmd::SendBody(parts.clone()))  // Bytes refcount bump only
```

### Sans-IO State Machines

Protocol logic (ZMTP session, frame decoder) is pure - no `async`, no IO traits. This allows deterministic testing, runtime swapping, and protocol evolution without refactoring.

### Feature-Gated Architecture

```rust
// Cargo.toml - protocols are opt-in
[dependencies]
monocoque-rs = { version = "0.1", features = ["zmq"] }

// Future: multiple protocols coexist
monocoque-rs = { features = ["zmq", "mqtt", "amqp"] }
```

Benefits: zero unused code compiled, clean dependency boundaries, protocol evolution without kernel changes.

### Performance Priorities

1. Syscall minimization (vectored writes, batching)
2. Cache locality (sorted arrays over pointer-heavy structures)
3. Zero-copy everywhere (`Bytes`, not `Vec<u8>`)
4. Predictable latency (no unbounded loops, early exits)

See blueprint 02 sections 7-8 for the IO performance model.

## What NOT to Do

- No `unsafe` outside `alloc/` module
- No Tokio-specific APIs (`tokio::spawn`, `tokio::select!`)
- No merging of protocol and IO logic (breaks testability)
- No tries/hashmaps for PUB/SUB (use sorted prefix table per blueprint 05)
- No web framework features (this is a messaging kernel, not REST)

## Key Files

```
monocoque/              # Public API crate
├── src/
│   ├── lib.rs         # Feature-gated protocol exports
│   └── zmq/
│       └── mod.rs     # DealerSocket, RouterSocket wrappers
└── examples/

monocoque-zmtp/         # ZMTP protocol implementation
├── src/
│   ├── session.rs     # Sans-IO state machine
│   ├── codec.rs       # Frame encoder/decoder
│   ├── dealer.rs      # DEALER socket
│   ├── router.rs      # ROUTER socket
│   ├── publisher.rs   # PUB socket
│   ├── subscriber.rs  # SUB socket
│   ├── integrated_actor.rs
│   └── multipart.rs

monocoque-core/         # Protocol-agnostic kernel
├── src/
│   ├── alloc.rs       # ONLY unsafe code - Page, SlabMut, IoArena
│   ├── actor.rs       # SocketActor split pumps
│   ├── router.rs      # RouterHub
│   ├── backpressure.rs
│   ├── error.rs
│   └── pubsub/
│       ├── hub.rs
│       ├── index.rs
│       └── mod.rs
```

## Dependencies

- `compio` (IO): io_uring/IOCP abstraction
- `flume` (channels): runtime-agnostic, SPSC/MPSC
- `bytes` (zero-copy): refcounted message buffers
- `smallvec` (stack optimization): avoid heap for small peer lists
- `hashbrown` (maps): fast hash maps for routing tables
- `futures` (select): runtime-agnostic multiplexing

## CHANGELOG Maintenance

Update `CHANGELOG.md` when completing features, fixing bugs, making API changes, or improving performance. All entries go under `[Unreleased]` until publication. Use clear, user-facing language - not internal implementation details.
