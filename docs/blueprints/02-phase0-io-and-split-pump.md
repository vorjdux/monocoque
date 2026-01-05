# 📄 File 3 — `02-phase0-io-and-split-pump.md`

# Phase 0 & 0.2 — Core IO and the Split Pump

_Cancellation-safe, ownership-correct, kernel-efficient I/O_

---

## 1. What Phase 0 Actually Solves

Phase 0 is **not** about ZMQ, routing, or messaging semantics.

It answers a more fundamental question:

> How do we move bytes between the kernel and user space **without deadlocks, UB, or hidden latency**?

The answer is the **Split Pump**.

---

## 2. The Split Pump Architecture

Instead of a single async loop that does everything, Monocoque splits responsibilities:

```
          ┌──────────────┐
          │   Read Pump  │  ← kernel → user
          └──────┬───────┘
                 │
            ZmtpSession
                 │
          ┌──────┴───────┐
          │  Write Pump  │  → user → kernel
          └──────────────┘
```

### Why this matters

-   **Cancellation safety**: cancelling write does not poison read
-   **Backpressure clarity**: each direction has independent flow control
-   **Ownership clarity**: buffers move in one direction only
-   **Zero shared mutable state** between pumps

This is the foundational decision that makes everything else robust.

---

## 3. Read Pump: Kernel → Session → Actor

### Responsibilities

1. Acquire a fresh `SlabMut`
2. Issue async read via `compio`
3. Receive ownership back
4. Convert to immutable `Bytes`
5. Feed into `ZmtpSession::on_bytes`

### Key properties

-   **No buffering policy here** Read pump does not aggregate frames or messages.

-   **No protocol knowledge** It only deals in bytes.

-   **Strict ownership discipline** `SlabMut` is:

    -   moved into IO
    -   returned
    -   frozen
    -   never reused while referenced

### Pseudocode (conceptual)

```rust
loop {
    let slab = arena.alloc();
    let (res, slab) = reader.read(slab).await;
    let n = res?;
    let bytes = slab.freeze(n);
    session.on_bytes(bytes);
}
```

---

## 4. Write Pump: Actor → Kernel

### Responsibilities

1. Receive framed `Bytes` from channel
2. Apply byte-based backpressure (`BytePermits`)
3. Opportunistically batch
4. Perform vectored writes
5. Handle partial writes correctly
6. Release permits

### Why it’s “dumb and fast”

The write pump:

-   does **not** know about ZMQ
-   does **not** know about multipart
-   does **not** know about identities

It just sends bytes.

This is intentional.

---

## 5. Ownership-Passing `write_vectored`

This was one of the most critical correctness points.

### Why ownership passing matters

With `io_uring`, once you submit a write:

-   the kernel may access buffers _after_ the syscall returns
-   the memory must remain valid and unmoved

Therefore:

> **The kernel must own the buffers during the write.**

`compio` enforces this by:

-   taking ownership of `Vec<Bytes>`
-   returning it after completion

The `flush_vectored_all` implementation must handle this correctly.

---

## 6. Partial Write Handling (The Classic Trap)

Non-blocking vectored writes **can and will**:

-   write only part of the batch
-   split inside a buffer
-   return `n < total_len`

### The correct algorithm

1. Submit batch
2. Kernel reports `n` bytes written
3. Advance logical cursor:

    - fully written buffers → drop
    - partially written buffer → slice

4. Retry with remaining buffers

This avoids:

-   data duplication
-   data loss
-   infinite loops

---

## 7. Why This Beats “One Syscall Per Message”

Compared to naive async IO:

| Aspect         | Naive         | Split Pump  |
| -------------- | ------------- | ----------- |
| Syscalls       | 1 per message | 1 per batch |
| Cancellation   | fragile       | isolated    |
| Partial writes | often broken  | correct     |
| Backpressure   | implicit      | explicit    |
| CPU cache      | poor          | friendly    |

This is **systems-grade IO**, not framework IO.

---

## 8. Backpressure: BytePermits

Instead of:

-   message counts
-   queue lengths

Monocoque uses **byte-based permits**.

Why?

-   kernel IO cost scales with bytes
-   batching amplifies this effect
-   prevents “one giant message” from starving others

### Properties

-   async acquire
-   synchronous release
-   pluggable (NoOp → Semaphore → dynamic policy)

This becomes essential later for:

-   PUB/SUB fanout
-   ROUTER fairness
-   memory pressure control

---

## 9. Cancellation Semantics (Why This Is Safe)

If:

-   user task is cancelled
-   peer disconnects
-   hub drops sender

Then:

-   read pump naturally exits on read error
-   write pump exits on channel close
-   no shared locks are poisoned
-   no buffer is leaked

This is why **Split Pump** is not optional.

---

## 10. Phase 0 Exit Criteria

Phase 0 will be complete when implementation satisfies:

-   [ ] No shared mutable state between read/write
-   [ ] No blocking in async paths
-   [ ] Correct handling of partial IO
-   [ ] Ownership-safe kernel interaction
-   [ ] No protocol logic in IO layer

The design ensures all criteria are architecturally achievable.
