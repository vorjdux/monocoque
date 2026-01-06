# 📄 File 6 — `05-phase3-pubsub-and-subscription-index.md`

# Phase 3 — PUB / SUB & the Sorted Prefix Engine

_From point-to-point messaging to high-fanout event distribution_

---

## 1. Why Phase 3 Exists

With Phase 2 complete, Monocoque already supports:

-   reliable framing
-   routing
-   identity
-   load balancing
-   reconnect safety

What it **does not** yet support is _selective fanout_.

That is the defining property of PUB/SUB.

Phase 3 answers:

> How do we route **one message to many peers**, filtered by topic, without destroying cache locality or Rust safety?

---

## 2. Constraints That Shape the Design

Before choosing a data structure, we locked in constraints:

1. **Typical ZMQ deployments**

    - < 10k topics
    - low fanout per topic
    - many publishes, fewer subscription changes

2. **Performance priorities**

    - fast publish hot path
    - predictable memory access
    - no per-message allocation
    - no locks

3. **Correctness priorities**

    - no ghost peers
    - no stale subscriptions
    - reconnect-safe
    - epoch-aware

---

## 3. Why Not a Trie?

A classic PUB/SUB solution is a trie.

We rejected it deliberately.

### Trie drawbacks in this context

-   pointer-heavy
-   cache-unfriendly
-   complex mutation logic
-   hard to epoch-clean safely
-   expensive for small N

Tries shine at **very large cardinality** (100k+ prefixes).

ZeroMQ workloads usually are not there.

---

## 4. Chosen Structure: Sorted Prefix Table

### Core idea

Store all prefixes in a single sorted vector:

```text
["", "A", "AB", "ABC", "B", "BTC", "BTC/USD"]
```

Each entry maps to a small list of peers.

### Why this works

-   prefixes are compared lexicographically
-   `prefix > topic` ⇒ cannot match
-   linear scan with early exit
-   extremely cache-friendly

---

## 5. SubscriptionIndex (The Math Core)

### Data Model

```rust
struct Subscription {
    prefix: Bytes,
    peers: SmallVec<[PeerKey; 4]>,
}
```

Key decisions:

-   `Bytes` for zero-copy prefixes
-   `SmallVec` to keep common cases stack-only
-   `PeerKey = u64` for compact indexing

---

### Operations & Complexity

| Operation          | Complexity                    | Notes                  |
| ------------------ | ----------------------------- | ---------------------- |
| subscribe          | O(log N)                      | binary search + insert |
| unsubscribe        | O(log N)                      | binary search          |
| disconnect cleanup | O(N)                          | acceptable on churn    |
| publish            | O(N) worst, early-exit common | hot path               |

---

## 6. Matching Algorithm (Hot Path)

```rust
for prefix in subs {
    if prefix > topic {
        break;
    }
    if topic.starts_with(prefix) {
        emit(peers);
    }
}
```

Properties:

-   branch-predictable
-   linear memory scan
-   no recursion
-   no heap allocation
-   deterministic

This is exactly what modern CPUs like.

---

## 7. Deduplication Semantics

A peer may subscribe to:

-   `"A"`
-   `"AB"`

Publishing `"ABC"` matches both.

Therefore:

-   duplicates must be removed
-   happens **after** matching
-   uses `sort_unstable + dedup`

This only runs when a peer subscribes redundantly — rare.

---

## 8. Epoch Safety (Ghost Peer Fix, Reused)

PUB/SUB reuses the **epoch model** from Phase 2.

### Why this matters more here

Subscriptions can outlive connections.

Without epochs:

-   reconnects resurrect old subscriptions
-   fanout targets dead peers
-   memory grows silently

### Epoch rules (unchanged)

-   `PeerUp(epoch)` overwrites
-   `PeerDown(epoch)` ignored if stale
-   cleanup only on matching epoch

---

## 9. PubSubHub (The Supervisor)

The hub bridges:

-   actors
-   subscription index
-   user publishing

### Responsibilities

-   map `RoutingID → PeerKey`
-   manage epochs
-   apply SUB / UNSUB commands
-   fanout published messages

### Non-responsibilities

-   parsing frames
-   decoding ZMTP
-   touching IO buffers

---

## 10. Zero-Copy Fanout

Critical design point:

```rust
tx.send(PeerCmd::SendBody(parts.clone()))
```

This:

-   clones the `Vec`
-   **does not copy payloads**
-   increments `Bytes` refcounts only

Fanout cost:

-   O(K) pointer copies
-   no memcpy
-   bounded and predictable

---

## 11. PUB / SUB Protocol Semantics

### SUB side

-   sends `SUB <prefix>`
-   sends `UNSUB <prefix>`

### PUB side

-   sends `[topic, body...]`
-   no routing envelopes
-   no identities

The hub enforces this contract.

---

## 12. Safety Analysis (Your Original Concern)

> “Will unsafe code break Rust’s guarantees?”

### Phase 3 answer: **No**

Why:

-   no raw pointers
-   no aliasing mutable memory
-   no interior mutability
-   `Bytes` enforces shared-immutable semantics
-   all mutation isolated in single-threaded hubs

Unsafe code exists only:

-   **below** this layer (io_uring, Slab)
-   fully encapsulated
-   already proven in Phase 0

Phase 3 itself is **100% safe Rust**.

---

## 13. Phase 3 Exit Criteria

**Status**: 🚧 **Skeleton Complete, Full Testing Pending**

Implementation progress:

-   ✅ PUB/SUB integrated actors implemented
-   ✅ Subscription index (sorted prefix table)
-   ✅ Linear scan matching with early exit
-   ✅ Ghost peer protection (epoch model reused)
-   ✅ Zero-copy fanout (`parts.clone()` = refcount bumps)
-   ✅ No unsafe code added (100% safe Rust)
-   ✅ Runtime-agnostic (flume channels)
-   ✅ `SmallVec` optimization for peer lists
-   🚧 Full integration tests (PUB → multiple SUBs)
-   🚧 Subscription churn stress tests
-   🚧 Fanout deduplication validation

**What remains**:

-   Integration tests with overlapping prefixes
-   Subscribe/unsubscribe churn testing
-   Fanout performance benchmarking
-   Memory usage profiling with many subscriptions

---

## 14. What Phase 3 Unlocks

After this phase:

-   PUB/SUB is production-ready
-   metrics streams are trivial
-   event buses are trivial
-   monitoring systems are trivial

The architecture does **not change** for later phases.
