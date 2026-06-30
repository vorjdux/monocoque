# 📄 File 5 - `04-phase2-router-dealer-and-load-balancing.md`

# Phase 2 - ROUTER / DEALER Semantics & Load Balancing

> **Implementation Note**: This document describes design concepts for routing and load balancing. The current implementation uses **direct stream I/O** where each socket (`DealerSocket<S>`, `RouterSocket<S>`) directly manages its own stream. The semantic behavior (multipart, identity routing) remains as described, but is implemented inline within socket types. The `RouterHub` and related components exist in `monocoque-core` for future multi-peer scenarios.

_Where Monocoque stops being "a ZMTP peer" and becomes "a ZeroMQ engine"_

---

## 1. What Phase 2 Solves

Phase 2 answers the next structural question:

> Can Monocoque correctly implement **ZeroMQ socket behaviors** without corrupting the IO fast path or violating Rust's safety guarantees?

Specifically:

-   ROUTER
-   DEALER
-   multipart messages
-   identity routing
-   fair load balancing
-   reconnect safety

---

## 2. Architectural Design: Layered Responsibilities

The design uses **three-layer separation** for managing socket semantics:

```
┌───────────────┐
│  Socket Layer │  ← owns IO, session, framing
└───────┬───────┘
        │ events / commands
┌───────▼───────┐
│   Hub (Router)│  ← routing, peer maps, policies (optional, for multi-peer)
└───────┬───────┘
        │ messages
┌───────▼───────┐
│     User API  │  ← application-facing semantics
└───────────────┘
```

This avoids:

-   locks in the IO path
-   shared mutable state between peers
-   unsafe aliasing

---

## 3. Multipart Bridge (The Missing Link)

### The Problem

ZMTP frames are **not messages**.

A message may consist of:

-   1 frame
-   N frames (`MORE` flag)

Protocols that ignore this inevitably break ROUTER, DEALER, PUB/SUB.

---

### The Solution: `MultipartBuffer`

Responsibilities:

-   accumulate frames
-   track `MORE`
-   emit a complete `Vec<Bytes>`

Properties:

-   zero-copy (Bytes slicing)
-   bounded (frame count + byte size limits)
-   protocol-correct

This buffer lives in the **socket implementation**, not a separate hub.

---

## 4. DEALER Semantics

### Inbound (Peer → User)

-   pass-through
-   no envelopes
-   multipart preserved

### Outbound (User → Peer)

User sends:

```text
[Part1, Part2, ..., PartN]
```

Socket implementation emits:

```text
Frame(Part1, MORE=1)
Frame(Part2, MORE=1)
...
Frame(PartN, MORE=0)
```

### Key design choice

-   Framing happens before write
-   Write operations remain protocol-agnostic

This preserves:

-   batching
-   vectored writes
-   syscall minimization

---

## 5. ROUTER Semantics

ROUTER introduces **identity envelopes**.

### Inbound (Peer → User)

Actual wire format:

```text
[Body...]
```

User-visible format:

```text
[RoutingID, Empty, Body...]
```

Why the empty frame?

-   required by ZMQ conventions
-   keeps REQ/REP compatibility later

---

### Outbound (User → Router)

User sends:

```text
[RoutingID, Empty, Body...]
```

Hub:

-   strips envelope
-   routes body to correct peer

---

## 6. The Router Hub

The **Hub** is a supervisor, not an IO component.

### Responsibilities

-   peer lifecycle (up/down)
-   routing table
-   load balancing
-   policy enforcement

### Non-responsibilities

-   framing
-   decoding
-   socket IO
-   buffer ownership

This keeps it:

-   runtime-agnostic
-   testable
-   simple

---

## 7. Strict Type Separation (Critical Safety Decision)

To prevent envelope confusion, Phase 2 introduced **hard type boundaries**:

```rust
UserCmd   → carries routing envelope
PeerCmd   → carries body only
HubEvent  → lifecycle only
```

This prevents entire classes of bugs:

-   sending envelopes twice
-   forgetting to strip IDs
-   misrouting multipart frames

This is **type-level protocol correctness**.

---

## 8. Load Balancing (Server-Side DEALER Pattern)

ROUTER can operate in two modes:

### 8.1 Standard Mode

-   user specifies RoutingID
-   direct delivery
-   silent drop if peer missing (ZMQ spec)

### 8.2 LoadBalancer Mode

-   user sends body only
-   hub selects peer
-   round-robin distribution

This enables:

-   worker pools
-   fan-out services
-   REQ/REP-like patterns without REQ/REP complexity

---

## 9. The "Ghost Peer" Problem

### The Bug

-   peer disconnects
-   reconnects quickly
-   old state races with new state
-   messages routed to dead channels

This **will happen** in real systems.

---

### The Fix: Epochs

Each peer connection gets:

-   a monotonic `epoch: u64`

Rules:

-   `PeerUp(epoch)` replaces previous epoch
-   `PeerDown(epoch)` ignored if stale

Result:

-   no ghost peers
-   no stale cleanup
-   no unsafe shared state

This is a **distributed systems fix**, not just Rust hygiene.

---

## 10. Self-Healing Round Robin

The load balancer is **defensive**:

-   detects stale IDs
-   repairs the list on the fly
-   never panics
-   never loops forever

This matters because:

-   churn is normal
-   reconnections are frequent
-   correctness beats theoretical O(1)

---

## 11. Phase 2 Validation

### Verified via integration tests:

-   ROUTER ↔ DEALER interop with libzmq
-   multipart correctness
-   strict round-robin fairness
-   reconnect stability
-   no message loss

These tests run against:

-   real sockets
-   real ZMQ peers
-   real timing

---

## 12. Phase 2 Status

**Status**: ✅ **COMPLETE**

Implementation:

-   ✅ DEALER socket implemented
-   ✅ ROUTER socket implemented
-   ✅ Multipart semantics
-   ✅ Load balancing hub available in core (for future multi-peer)
-   ✅ Ghost peer protection (epoch model)
-   ✅ No unsafe code (100% safe Rust)
-   ✅ Type separation enforced
-   ✅ libzmq interop verified

**Future Work**:

-   Multi-peer scenarios using RouterHub
-   Reconnect stability with hub
-   Fair queueing under load
-   Load balancer self-healing in complex topologies

---

## 13. Why Phase 2 Establishes the Foundation

After Phase 2:

-   REQ/REP is straightforward
-   PUSH/PULL is straightforward
-   PUB/SUB is possible

This phase is where **most projects collapse**.

Monocoque didn't.
