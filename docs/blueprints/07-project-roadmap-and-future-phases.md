# 📄 File 8 - `07-project-roadmap-and-future-phases.md`

# Monocoque - Full Project Roadmap & Future Phases

_A complete, bounded plan from kernel to ecosystem_

---

## 1. What Monocoque Is (Final Definition)

**Monocoque is a high-performance messaging kernel**, not just a library.

It provides:

-   A **zero-copy, syscall-minimal IO core**
-   A **protocol-agnostic actor runtime**
-   A **ZeroMQ-compatible protocol stack**
-   A **foundation for future custom protocols**

Everything else (ZMQ, RPC, PUB/SUB) is _payload logic_ layered on top.

---

## 2. Implementation Status

### Crate Structure

**Monocoque uses a layered crate architecture**:

```
monocoque/              ← Public API (ergonomic socket types)
├── DealerSocket
├── RouterSocket
├── PubSocket
└── SubSocket

monocoque-zmtp/         ← ZMTP protocol (opt-in via features)
├── Session
├── Framing
├── Commands
├── Integrated Actors
└── (100% safe Rust)

monocoque-core/         ← Protocol-agnostic kernel
├── alloc/ (ONLY unsafe code)
├── router/ (Identity routing)
├── pubsub/ (Subscription index)
└── (feature-gated, no protocols by default)
```

### Phase 0 - Core Kernel

**Goal:** Move bytes between kernel and user space safely and fast.

**Status**: ✅ **COMPLETE**

Implemented components:

-   ✅ Slab / Arena allocator with refcounting
-   ✅ Stable IO buffers (`SlabMut` with `IoBufMut`)
-   ✅ **IoBytes wrapper** (zero-copy `Bytes` → `IoBuf`)
-   ✅ `io_uring` via `compio`
-   ✅ Direct stream I/O pattern
-   ✅ Cancellation-safe IO
-   ✅ Zero-copy `Bytes` pipeline
-   ✅ Partial write handling

---

### Phase 1 - ZMTP Core Protocol

**Goal:** Speak ZeroMQ at the frame level.

**Status**: ✅ **COMPLETE**

Implemented components:

-   ✅ ZMTP 3.1 framing (short/long)
-   ✅ Zero-copy fast path
-   ✅ Fragmented-frame decoder
-   ✅ Greeting + NULL handshake
-   ✅ Session state machine (Sans-IO)
-   ✅ READY command with metadata
-   ✅ Identity ownership (copy_from_slice)
-   🚧 Interop with libzmq (tests pending)

---

### Phase 2 - Socket Behaviors

**Goal:** Become a real ZeroMQ implementation.

**Status**: ✅ **Implementation Complete, Testing Pending**

Implemented components:

-   ✅ DEALER multipart logic
-   ✅ ROUTER identity envelopes
-   ✅ Hub + per-peer actors
-   ✅ Strict type separation (`UserCmd` vs `PeerCmd`)
-   ✅ Load-balancing router mode
-   ✅ Epoch-based ghost-peer prevention
-   ✅ Runtime-agnostic hub (flume)
-   🚧 Full integration tests pending

---

### Phase 3 - PUB/SUB Engine

**Goal:** High-performance topic-based fanout.

**Status**: ✅ **Implementation Complete, Testing Pending**

Implemented components:

-   ✅ Sorted Prefix Table (linear scan)
-   ✅ Cache-friendly matching algorithm
-   ✅ `SmallVec<[PeerKey; 4]>` fanout
-   ✅ Epoch-safe subscription lifecycle
-   ✅ Zero-copy broadcast (Vec clone, Bytes refcount)
-   ✅ PubSub hub architecture
-   ✅ SUB command parsing
-   ✅ PUB/SUB socket types
-   🚧 Full integration tests pending

---

### Public API Layer

**Goal:** Ergonomic, idiomatic Rust API for application developers.

**Status**: ✅ **COMPLETE**

```rust
// User-friendly API
use monocoque::zmq::DealerSocket;

let mut socket = DealerSocket::connect("127.0.0.1:5555").await?;
socket.send(vec![b"Hello".into()]).await?;
let reply = socket.recv().await;
```

Features:

-   ✅ Feature-gated protocols (`features = ["zmq"]`)
-   ✅ Zero default features (explicit opt-in)
-   ✅ Idiomatic async/await API
-   ✅ Re-exports of `Bytes` for convenience
-   ✅ Comprehensive documentation with examples

---

## 3. Immediate Priorities

### Phase 3.1 - PUB/SUB Integration Tests

-   Multiple SUB clients
-   Overlapping prefixes
-   Subscribe / unsubscribe churn
-   Peer reconnect safety
-   Interop with libzmq PUB

---

### Phase 3.2 - Socket Type Matrix

-   PUB
-   SUB
-   XPUB / XSUB (optional)
-   Behavior implemented in socket types

---

## 4. Phase 4 - High-Performance RPC (Optional, Strategic)

**Goal:** Beat gRPC on latency and CPU.

Planned:

-   FlatBuffers framing
-   Length-prefixed protocol
-   No HTTP/2
-   No HPACK
-   No headers
-   No dynamic dispatch
-   Alignment-aware reads

This is where Monocoque becomes more than ZMQ.

---

## 5. Phase 5 - Custom Protocol Mode (Your Question Answered)

> “Maybe create my own protocol in the future to outperform all?”

**Yes - and Monocoque is built for exactly that.**

Why it will work:

-   Protocol logic is **pure Sans-IO**
-   Transport is already optimal
-   Memory model already proven
-   No socket abstraction leakage
-   No runtime coupling

You can implement:

-   Binary RPC
-   Event streams
-   Market data feeds
-   Telemetry
-   Actor RPC
-   Custom pub/sub

All without touching the kernel.

---

## 6. Phase 6 - Transports Beyond TCP (Optional)

Possible extensions:

-   QUIC (datagram + stream)
-   Shared memory (IPC)
-   RDMA (advanced)
-   Unix Domain Sockets

The kernel design already supports this.

---

## 7. What Will NOT Be Added (Intentionally)

Monocoque will **not** become:

-   ❌ A web framework
-   ❌ A REST system
-   ❌ A generic async runtime
-   ❌ A kitchen-sink networking crate

This restraint is why it stays fast and correct.

---

## 8. Long-Term Stability Strategy

-   Phase 0–2 APIs stabilize early
-   Protocol layers evolve independently
-   Unsafe surface area remains fixed
-   Performance improvements are internal only
-   Compatibility with libzmq preserved

---

## 9. Naming Check (Final Answer)

### **Monocoque** - Is it good?

Yes. For technical audiences, it is **excellent**.

Why:

-   Structural metaphor (F1-grade)
-   Strong, rigid, minimal shell
-   Kernel-like connotation
-   Not overloaded in networking
-   Distinctive and memorable

It signals:

> _“This is not a framework. This is a chassis.”_

---

## 10. Final Strategic Verdict

**Is Monocoque worth developing?**

✅ Yes - technically ✅ Yes - architecturally ✅ Yes - strategically

You are not reinventing ZeroMQ. You are **rebuilding it correctly** for modern Rust, modern kernels, and modern performance expectations.

And unlike most ambitious projects:

-   the scope is controlled
-   the unsafe code is minimal
-   the design is coherent
-   the milestones are real

---

## 11. End of Roadmap

This concludes the **full project documentation** in 8 files.

If you want next:

-   🔍 a _formal unsafe proof document_
-   🧪 a _test strategy & fuzzing plan_
-   🚀 a _“how to open-source this” plan_
-   📊 or a _benchmark strategy vs libzmq / gRPC_

Just tell me.
