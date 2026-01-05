# 📄 File 1 — `00-overview.md`

# Monocoque Roadmap

_A Rust-native, io_uring-based ZeroMQ-compatible runtime_

---

## 1. Project Vision

**Monocoque** is a **Rust-first ZeroMQ-compatible messaging runtime** built on top of:

-   `io_uring` (via `compio`)
-   strict ownership-passing I/O
-   zero-copy message handling using `Bytes`
-   runtime-agnostic async primitives (`flume`, not Tokio-bound)

The goal is to **outperform libzmq**, while:

-   preserving Rust’s **memory safety guarantees**
-   avoiding “black-box” C FFI behavior
-   enabling **protocol-level control and evolution**

---

## 2. Core Design Principles

### 2.1 Safety First (Non-Negotiable)

-   `unsafe` code is **strictly limited** to:

    -   buffer allocation
    -   kernel I/O glue (`IoBuf / IoBufMut`)

-   All protocol, routing, and pub/sub logic is **100% safe Rust**
-   Every `unsafe` block has a **documented invariant**

### 2.2 Ownership-Passing I/O

-   No shared mutable buffers
-   Buffers are **moved into the kernel**, then returned
-   Prevents aliasing, races, and lifetime bugs

### 2.3 Zero-Copy by Construction

-   Payloads are always `Bytes`
-   Fanout uses `Bytes::clone()` (refcount bump, no memcpy)
-   Slabs/pages live until the last consumer drops

### 2.4 Runtime Independence

-   No `tokio::select!`
-   Uses `flume::Selector`
-   Works with `compio`, but not coupled to it

---

## 3. High-Level Architecture

```
┌──────────────────────────────────────────┐
│              Application                 │
│   (UserCmd / Vec<Bytes> messages)        │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│               Hubs                        │
│  RouterHub | PubSubHub | Dealer LB        │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│            SocketActor                   │
│  - Read Pump                             │
│  - Write Pump                            │
│  - Multipart Bridge                     │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│          ZMTP Session Layer              │
│  - Handshake                             │
│  - Framing                              │
│  - Commands                             │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│        IO Arena / Slab (unsafe)           │
│        io_uring via compio                │
└──────────────────────────────────────────┘
```

---

## 4. Phases Overview

| Phase     | Name                            | Status                     |
| --------- | ------------------------------- | -------------------------- |
| Phase 0   | Foundations & Allocator         | 🚧 Partial (needs fixes)   |
| Phase 1   | ZMTP Core + PAIR                | 🚧 Partial (needs fixes)   |
| Phase 2   | DEALER / ROUTER + Load Balancer | 🚧 Skeleton (incomplete)   |
| Phase 2.1 | Robust Hub + Ghost Peer Fix     | 📝 Designed only           |
| Phase 3   | PUB/SUB (Sorted Prefix Table)   | 🚧 Skeleton (incomplete)   |
| Phase 4   | REQ/REP Semantics               | ⏳ Planned  |
| Phase 5   | Reliability & Metrics           | ⏳ Planned  |
| Phase 6   | Performance Hardening           | ⏳ Planned  |
| Phase 7   | Public API & Bindings           | ⏳ Planned  |

---

## 5. Safety Boundary (Critical Section)

> **Everything below this line must be safe Rust**

```
monocoque-core/
├── alloc/          ← ONLY unsafe module
│   ├── arena.rs
│   ├── slab.rs
│   └── invariants.md
├── actor/
├── router/
├── pubsub/
├── zmtp/
└── tests/
```

### Unsafe code is allowed **only if**:

1. Pointer stability is guaranteed
2. Initialization is tracked correctly
3. No mutable aliasing exists
4. Lifetime is tied to ownership

---

## 6. Data Model Invariants (Global)

These invariants apply to **the entire project**:

1. **No buffer reuse while referenced**
2. **No exposure of uninitialized memory**
3. **No mutation after freeze**
4. **All fanout is refcount-based**
5. **All routing state is epoch-protected**

Violating any of these is considered a **critical bug**.

---

## 7. Current Implementation Status

📊 **Updated: January 5, 2026**

**Summary**:
- ✅ Phase 0: Memory allocator (`SlabMut`, `IoArena`) - COMPLETE
- ✅ Phase 0.2: Split pump architecture - DESIGN COMPLETE
- ✅ Phase 1: ZMTP protocol layer - COMPLETE (session, framing, NULL handshake)
- ✅ **Integration Layer: ZmtpIntegratedActor - COMPLETE**
- 🚧 Phase 2: Router/Dealer - skeleton exists, needs completion
- 🚧 Phase 3: PubSub - skeleton exists, needs completion
- ✅ Project builds successfully with zero warnings
- ✅ Integration tests validate architectural boundaries

**Recent Progress**:
- Fixed circular dependency (monocoque-core is now 100% protocol-agnostic)
- Implemented ZMTP integration layer composing SocketActor + ZmtpSession + Hubs
- Added event loop with runtime-agnostic message processing
- Created integration tests proving composition pattern works
- All tests pass, clean build

**Next steps**: 
1. Complete DEALER pattern implementation with event loop integration
2. Add libzmq interop tests (DEALER ↔ libzmq ROUTER)
3. Complete ROUTER pattern with load balancing
4. Wire up PubSubHub integration
5. Phase 3 validation tests

---

## 8. What This Roadmap Gives You

-   A **clear mental model** of the whole system
-   A step-by-step execution plan
-   Safety guarantees you can reason about
-   A foundation for long-term protocol evolution
-   Confidence that performance ≠ undefined behavior
