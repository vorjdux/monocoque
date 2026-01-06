# Monocoque Copilot Instructions

## Project Overview
Monocoque is a high-performance, Rust-native ZeroMQ-compatible messaging runtime built on `io_uring` (via `compio`). **Status**: Core implementation complete (Phase 0-1), socket types implemented (Phase 2-3 skeleton), integration testing pending. Comprehensive blueprints in `docs/blueprints/`.

## Core Architecture (Read These First)
- `docs/blueprints/00-overview.md` - System architecture and phases
- `docs/blueprints/01-unsafe-boundary-and-allocator.md` - Safety model
- `docs/blueprints/06-safety-model-and-unsafe-audit.md` - Memory guarantees

**Key insight**: This is a layered messaging **kernel**, not a framework. Protocol logic is pure, IO is isolated.

## Critical Safety Rules (Non-Negotiable)

### Unsafe Code Boundary
- `unsafe` is ONLY allowed in: `monocoque-core/src/alloc.rs` (single file containing all allocation logic)
- Everything above Phase 0 (protocol, routing, pubsub) MUST be 100% safe Rust
- Every `unsafe` block requires documented invariants (see blueprint 01)

### Memory Invariants (Global)
1. No buffer reuse while referenced
2. No uninitialized memory exposure  
3. No mutation after freeze (`SlabMut` → `Bytes`)
4. All fanout is refcount-based (via `Bytes::clone()`)
5. All routing state is epoch-protected (ghost peer prevention)

Violating these = critical bug. See blueprint 06 for formal proofs.

## Implementation Phases

### Phase 0 - IO Core ✅ **COMPLETE** (January 2026)
**Components**: 
- `SlabMut` with `IoBufMut` trait implementation
- `IoBytes` wrapper for zero-copy writes (eliminates `.to_vec()` memcpy)
- Arena allocator with refcounting
- Split read/write pumps in `SocketActor`
- Partial write handling for vectored IO

**Pattern**: Ownership-passing IO - buffers move into kernel, return on completion
```rust
// Read pump pattern
let slab = arena.alloc();
let (res, slab) = reader.read(slab).await;  // kernel owns buffer
let bytes = slab.freeze(n);                  // convert to immutable

// Write pump pattern (zero-copy)
let io_buf = IoBytes::new(bytes);           // wrap Bytes for IoBuf
stream.write_all(io_buf).await;             // no memcpy!
```
**Critical**: Vectored writes MUST handle partial writes (see blueprint 02 §6)

### Phase 1 - ZMTP Protocol ✅ **COMPLETE** (January 2026)
**Components**: 
- Sans-IO `ZmtpSession` state machine (Greeting → Handshake → Active)
- Frame encoder/decoder with fragmentation support
- NULL mechanism handshake
- READY command with Socket-Type metadata
- Identity ownership via `Bytes::copy_from_slice`

**Pattern**: Pure state machine - `Bytes in → Events out` (no IO, no runtime)
**Status**: Protocol layer complete, libzmq interop tests pending
**Critical**: READY message MUST include `Socket-Type` metadata or libzmq silently drops peer

### Phase 2 - Routing 🚧 **SKELETON COMPLETE** (January 2026)
**Components**: 
- ✅ `ZmtpIntegratedActor` composing SocketActor + Session + Hubs
- ✅ DEALER socket with multipart bridge
- ✅ ROUTER socket with identity envelopes
- ✅ `RouterHub` with round-robin load balancing
- ✅ Epoch-based ghost peer prevention
- 🚧 Full integration tests pending
- 🚧 libzmq interop tests pending

**Pattern**: Three-layer separation - `SocketActor` (IO) → `Hub` (routing) → `User API`
**Critical**: Strict type boundaries - `UserCmd` (with envelope) vs `PeerCmd` (body only)

### Phase 3 - PUB/SUB 🚧 **SKELETON COMPLETE** (January 2026)
**Components**: 
- ✅ `SubscriptionIndex` with sorted prefix table
- ✅ PUB socket (broadcast send-only)
- ✅ SUB socket (subscribe/unsubscribe/recv)
- ✅ `PubSubHub` with epoch tracking
- ✅ Zero-copy fanout (Vec clone, Bytes refcount)
- 🚧 Full integration tests pending
- 🚧 Subscription matching validation pending

**Pattern**: Linear scan with early exit - cache-friendly, no per-message allocation
**Data structure**: `Vec<Subscription>` sorted by prefix, `SmallVec<[PeerKey; 4]>` per prefix

### Public API Layer ✅ **COMPLETE** (January 2026)
**Crate**: `monocoque` (ergonomic facade)
**Features**:
- ✅ Feature-gated protocols: `monocoque = { version = "0.1", features = ["zmq"] }`
- ✅ Zero default features (explicit opt-in)
- ✅ Idiomatic async/await API
- ✅ Protocol namespace: `monocoque::zmq::{DealerSocket, RouterSocket, PubSocket, SubSocket}`
- ✅ Comprehensive documentation with examples

**Usage**:
```rust
use monocoque::zmq::DealerSocket;

let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
socket.send(vec![b"Hello".into()]).await?;
let reply = socket.recv().await;
```

## Development Workflows

### Testing Strategy (Multi-Layered)
1. **Unit tests**: Deterministic, safe Rust logic only (12 tests passing)
2. **Interop tests**: Against real `libzmq` peers (validates protocol correctness) - **PENDING**
3. **Stress tests**: Reconnection churn, fanout, race conditions - **PENDING**
4. **Sanitizers**: AddressSanitizer (use-after-free), ThreadSanitizer (races) - **PENDING**

**Current Status**: Core unit tests pass, integration tests need setup
Run tests: `cargo test --workspace --features zmq`
Run interop (when ready): `cargo test --test interop_pair`

### Build Conventions
- Use `flume` for channels (runtime-agnostic, not Tokio-bound)
- Use `compio` for IO (io_uring/IOCP abstraction)
- Use `bytes` crate for zero-copy message handling
- NO `tokio::select!`, NO shared mutable state, NO `Arc<Mutex<T>>` in hot paths

## Project-Specific Patterns

### Epoch-Based Lifecycle
```rust
// Prevent ghost peer races on reconnect
struct PeerState { epoch: u64, tx: Sender<PeerCmd> }
// PeerUp(epoch) replaces old state
// PeerDown(epoch) ignored if stale
```
Used in: ROUTER hub (Phase 2), PUB/SUB subscriptions (Phase 3)

### Zero-Copy Fanout
```rust
// PUB/SUB broadcast - clone Vec, NOT payloads
tx.send(PeerCmd::SendBody(parts.clone()))  // Bytes refcount bump only
```

### Sans-IO State Machines
Protocol logic (ZMTP session, frame decoder) is pure - no `async`, no IO traits.
Allows: deterministic testing, runtime swapping, protocol evolution without refactoring.

### Feature-Gated Architecture (New in January 2026)
```rust
// Cargo.toml - protocols are opt-in
[dependencies]
monocoque = { version = "0.1", features = ["zmq"] }  # only ZMQ loaded

// Future: multiple protocols coexist
monocoque = { features = ["zmq", "mqtt", "amqp"] }
```

**Benefits**:
- Zero unused code compiled
- Clean dependency boundaries  
- Protocol evolution without kernel changes
- `monocoque-core` is 100% protocol-agnostic

### Recent Performance Optimizations (January 2026)
1. **IoBytes wrapper**: Eliminates `.to_vec()` memcpy on every write (~10-30% CPU reduction)
2. **Single-clone optimization**: Router/PubSub hubs minimized clones (1 clone + 1 move vs 2 clones)
3. **Move semantics**: Multipart buffer uses ownership transfer instead of clone
4. **Zero-copy fanout**: PUB/SUB clones Vec (cheap), Bytes are refcounted (no payload copy)

## What NOT to Do

❌ Add `unsafe` outside `alloc/` module  
❌ Use Tokio-specific APIs (`tokio::spawn`, `tokio::select!`)  
❌ Merge protocol and IO logic (breaks testability)  
❌ Implement tries/hashmaps for PUB/SUB (use sorted prefix table per blueprint 05)  
❌ Add web framework features (this is a messaging kernel, not REST)

## Key Files & Dependencies

**Current structure** (as of January 2026):
```
monocoque/              # Public API crate
├── src/
│   ├── lib.rs         # Feature-gated protocol exports
│   └── zmq/
│       └── mod.rs     # DealerSocket, RouterSocket wrappers
└── examples/
    └── protocol_namespaces.rs

monocoque-zmtp/         # ZMTP protocol implementation
├── src/
│   ├── session.rs     # Sans-IO state machine (✅ complete)
│   ├── codec.rs       # Frame encoder/decoder (✅ complete)
│   ├── dealer.rs      # DEALER socket (✅ skeleton)
│   ├── router.rs      # ROUTER socket (✅ skeleton)
│   ├── publisher.rs   # PUB socket (✅ skeleton)
│   ├── subscriber.rs  # SUB socket (✅ skeleton)
│   ├── integrated_actor.rs  # Composition layer (✅ complete)
│   └── multipart.rs   # Multipart buffer (✅ complete)

monocoque-core/         # Protocol-agnostic kernel
├── src/
│   ├── alloc.rs       # ONLY unsafe code (✅ complete)
│   │                  # Contains: Page, SlabMut, IoBytes, IoArena
│   ├── actor.rs       # SocketActor split pumps (✅ complete)
│   ├── router.rs      # RouterHub (✅ skeleton)
│   ├── backpressure.rs # BytePermits trait (✅ complete)
│   ├── error.rs       # Error types (✅ complete)
│   └── pubsub/
│       ├── hub.rs     # PubSubHub (✅ skeleton)
│       ├── index.rs   # SubscriptionIndex (✅ complete)
│       └── mod.rs     # Module exports
```

**Dependencies**:
- `compio` (IO): io_uring/IOCP abstraction
- `flume` (channels): runtime-agnostic, SPSC/MPSC
- `bytes` (zero-copy): refcounted message buffers
- `smallvec` (stack optimization): avoid heap for small peer lists
- `hashbrown` (maps): fast hash maps for routing tables
- `futures` (select): runtime-agnostic multiplexing

## Communication Patterns

- **Actor ↔ Hub**: Async channels (`UserCmd`, `PeerCmd`, `HubEvent`)
- **Hub ↔ Index**: Direct calls (single-threaded supervisor)
- **Kernel ↔ Rust**: Ownership-passing through `IoBuf`/`IoBufMut` traits

## Performance Priorities

1. Syscall minimization (vectored writes, batching)
2. Cache locality (sorted arrays over pointer-heavy structures)
3. Zero-copy everywhere (`Bytes`, not `Vec<u8>`)
4. Predictable latency (no unbounded loops, early exits)

Read blueprint 02 §7-8 for IO performance model.

## When in Doubt

1. Check blueprints - they contain formal proofs and rationale (updated January 2026)
2. Prioritize safety over performance (but architecture provides both)
3. Maintain Sans-IO purity for protocol logic
4. Document any new `unsafe` with invariants (but prefer not adding)
5. **Run tests after changes**: `cargo test --workspace --features zmq`
6. **Check for blueprint violations**: All protocol code must be 100% safe Rust

**Current Priority**: Integration testing with libzmq to validate protocol correctness

**Philosophy**: Performance through correct architecture, not through unsafe shortcuts.
