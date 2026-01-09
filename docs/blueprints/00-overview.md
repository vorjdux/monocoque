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

### 2.5 Feature-Gated Protocols

-   Protocols are **opt-in** via Cargo features
-   No default features (explicit dependencies only)
-   `monocoque-core` is 100% protocol-agnostic
-   Example: `monocoque = { version = "0.1", features = ["zmq"] }`

This ensures:

-   Zero unused code compiled
-   Clean dependency boundaries
-   Protocol evolution without kernel changes

---

## 3. High-Level Architecture

```
┌──────────────────────────────────────────┐
│              Application                 │
│   (DealerSocket, RouterSocket, etc.)      │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│         ZMTP Socket Layer               │
│  (Direct stream I/O implementation)      │
│  - Handshake                             │
│  - Framing                               │
│  - Multipart assembly                    │
│  - Generic over AsyncRead+AsyncWrite     │
└──────────────────────────────────────────┘
                 │
                 ▼
┌──────────────────────────────────────────┐
│        IO Arena / Slab (unsafe)           │
│        + Buffer Management                 │
│        + Transport Utilities               │
│        io_uring via compio                 │
└──────────────────────────────────────────┘
```

---

## 4. Phases Overview

| Phase     | Name                            | Status                   |
| --------- | ------------------------------- | ------------------------ |
| Phase 0   | Foundations & Allocator         | 🚧 Partial (needs fixes) |
| Phase 1   | ZMTP Core + PAIR                | 🚧 Partial (needs fixes) |
| Phase 2   | DEALER / ROUTER + Load Balancer | 🚧 Skeleton (incomplete) |
| Phase 2.1 | Robust Hub + Ghost Peer Fix     | 📝 Designed only         |
| Phase 3   | PUB/SUB (Sorted Prefix Table)   | 🚧 Skeleton (incomplete) |
| Phase 4   | REQ/REP Semantics               | ⏳ Planned               |
| Phase 5   | Reliability & Metrics           | ⏳ Planned               |
| Phase 6   | Performance Hardening           | ⏳ Planned               |
| Phase 7   | Public API & Bindings           | ⏳ Planned               |

---

## 5. Safety Boundary (Critical Section)

> **Everything below this line must be safe Rust**

```
monocoque-core/
├── alloc/          ← ONLY unsafe module
│   ├── arena.rs
│   ├── slab.rs
│   └── invariants.md
├── router/
├── pubsub/
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

## 7. Implementation Status

### Core Infrastructure

The foundational layers of Monocoque are complete:

**Phase 0 - Memory Allocator & IO Kernel** ✅

-   Slab/Arena allocator with refcounting
-   Zero-copy buffer management (`SlabMut` → `Bytes`)
-   `SegmentedBuffer` for efficient receive buffering
-   `io_uring` integration via compio

**Phase 1 - ZMTP Protocol Layer** ✅

-   ZMTP 3.1 framing (short/long format)
-   Greeting and NULL handshake
-   Session state machine (Sans-IO)
-   READY command processing
-   Frame codec for encoding/decoding

**Phase 2-4 - Socket Implementations** ✅

-   Direct stream I/O architecture (generic over AsyncRead+AsyncWrite)
-   Each socket manages its own handshake, decoding, and multipart assembly
-   DEALER: Asynchronous request-reply
-   ROUTER: Identity-based routing
-   PUB/SUB: Event distribution with subscription filtering
-   REQ/REP: Synchronous request-reply patterns
-   All sockets support TCP and Unix domain sockets

### Testing & Validation

**Current State:**

-   ✅ Unit tests passing (12 tests)
-   ✅ Clean build with zero warnings
-   ✅ Safety model validated (unsafe code isolated)
-   🚧 Integration tests with libzmq pending

### Architecture

```
┌─────────────────────────────────────┐
│   monocoque-zmtp (sockets)          │  ← Direct stream I/O
│  DealerSocket, RouterSocket, etc.   │     Generic over streams
└──────────────┬──────────────────────┘
               │
┌──────────────┼──────────────────────┐
│              │                      │
│  Protocol    │  Core Utilities      │
│  (handshake, │  (alloc, buffer,     │
│   codec)     │   endpoint, config)  │
│              │                      │
└──────────────┴──────────────────────┘
```

### Future Work

1. Complete interoperability tests (DEALER ↔ ROUTER validation)
2. PUB/SUB integration tests with subscription matching
3. Stress tests (reconnection churn, fanout)
4. Performance benchmarking vs libzmq

---

## 8. What This Roadmap Gives You

-   A **clear mental model** of the whole system
-   A step-by-step execution plan
-   Safety guarantees you can reason about
-   A foundation for long-term protocol evolution
-   Confidence that performance ≠ undefined behavior
