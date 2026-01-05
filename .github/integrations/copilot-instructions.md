# Monocoque Copilot Instructions

## Project Overview
Monocoque is a high-performance, Rust-native ZeroMQ-compatible messaging runtime built on `io_uring` (via `compio`). Currently in design phase with comprehensive blueprints in `docs/blueprints/`.

## Core Architecture (Read These First)
- `docs/blueprints/00-overview.md` - System architecture and phases
- `docs/blueprints/01-unsafe-boundary-and-allocator.md` - Safety model
- `docs/blueprints/06-safety-model-and-unsafe-audit.md` - Memory guarantees

**Key insight**: This is a layered messaging **kernel**, not a framework. Protocol logic is pure, IO is isolated.

## Critical Safety Rules (Non-Negotiable)

### Unsafe Code Boundary
- `unsafe` is ONLY allowed in: `monocoque-core/src/alloc/{slab.rs,arena.rs}`
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

### Phase 0 - IO Core (Foundation)
**Components**: `SlabMut`, Arena allocator, Split read/write pumps
**Pattern**: Ownership-passing IO - buffers move into kernel, return on completion
```rust
// Read pump pattern
let slab = arena.alloc();
let (res, slab) = reader.read(slab).await;  // kernel owns buffer
let bytes = slab.freeze(n);                  // convert to immutable
```
**Critical**: Vectored writes MUST handle partial writes (see blueprint 02 §6)

### Phase 1 - ZMTP Protocol  
**Components**: Sans-IO session state machine, framing (`encode_frame`), handshake
**Pattern**: Pure state machine - `Bytes in → Events out` (no IO, no runtime)
**Critical**: READY message MUST include `Socket-Type` metadata or libzmq silently drops peer

### Phase 2 - Routing
**Components**: ROUTER/DEALER hubs, multipart bridge, load balancer
**Pattern**: Three-layer separation - `SocketActor` (IO) → `Hub` (routing) → `User API`
**Critical**: Strict type boundaries - `UserCmd` (with envelope) vs `PeerCmd` (body only)

### Phase 3 - PUB/SUB (Current)
**Components**: Sorted Prefix Table (not trie), epoch-safe subscriptions
**Pattern**: Linear scan with early exit - cache-friendly, no per-message allocation
**Data structure**: `Vec<Subscription>` sorted by prefix, `SmallVec<[PeerKey; 4]>` per prefix

## Development Workflows

### Testing Strategy (Multi-Layered)
1. **Unit tests**: Deterministic, safe Rust logic only
2. **Interop tests**: Against real `libzmq` peers (validates protocol correctness)
3. **Stress tests**: Reconnection churn, fanout, race conditions
4. **Sanitizers**: AddressSanitizer (use-after-free), ThreadSanitizer (races)

Run interop: `cargo test --test libzmq_interop` (when implemented)

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

## What NOT to Do

❌ Add `unsafe` outside `alloc/` module  
❌ Use Tokio-specific APIs (`tokio::spawn`, `tokio::select!`)  
❌ Merge protocol and IO logic (breaks testability)  
❌ Implement tries/hashmaps for PUB/SUB (use sorted prefix table per blueprint 05)  
❌ Add web framework features (this is a messaging kernel, not REST)

## Key Files & Dependencies (When Implemented)

Expected structure:
```
monocoque-core/
├── alloc/          # ONLY unsafe code
├── actor/          # SocketActor, split pumps
├── router/         # ROUTER/DEALER hubs
├── pubsub/         # SubscriptionIndex
└── zmtp/           # Protocol state machines
```

External: `compio` (IO), `flume` (channels), `bytes` (zero-copy), `smallvec` (stack optimization)

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

1. Check blueprints - they contain formal proofs and rationale
2. Prioritize safety over performance (but architecture provides both)
3. Maintain Sans-IO purity for protocol logic
4. Document any new `unsafe` with invariants (but prefer not adding)

**Philosophy**: Performance through correct architecture, not through unsafe shortcuts.
