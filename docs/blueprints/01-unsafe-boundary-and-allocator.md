# 📄 File 2 — `01-unsafe-boundary-and-allocator.md`

# Unsafe Boundary & Allocator

_How Monocoque uses `unsafe` without breaking Rust’s guarantees_

---

## 1. Why Unsafe Exists Here (and Only Here)

Monocoque needs unsafe code for exactly one reason:

> `io_uring` requires that memory passed to the kernel **does not move** while an operation is in flight.

Rust can’t express “this pointer stays stable while the kernel owns it” purely in the type system without either:

-   pinning + bespoke buffer types, or
-   an ownership-passing I/O API that enforces exclusivity (what `compio` does)

So we embrace a **tiny unsafe core** that provides:

-   stable memory
-   bounds correctness
-   init tracking
-   interop with `compio::buf::{IoBuf, IoBufMut, SetBufInit}`

Everything else remains safe.

---

## 2. The One Unsafe Module Rule

**Only these files may contain `unsafe`:**

-   `monocoque-core/src/alloc/slab.rs`
-   `monocoque-core/src/alloc/arena.rs`
-   `monocoque-core/src/alloc/invariants.md` (documentation only)

All other crates/modules:

-   `monocoque-core/src/router/*`
-   `monocoque-core/src/pubsub/*`
-   `monocoque-zmtp/*`

must be safe Rust only.

---

## 3. SlabMut: The Buffer Contract

### 3.1 Intended Semantics

`SlabMut` is a **mutable, kernel-safe buffer** that:

-   points to a stable allocation
-   can be written into by `io_uring`
-   later “freezes” into `Bytes` for zero-copy usage

Conceptually:

```
SlabMut (mutable, kernel-owned during IO)
    |
    | freeze(n)
    v
Bytes (immutable, shareable, refcounted)
    |
    | IoBytes::new(bytes)
    v
IoBytes (IoBuf wrapper for compio writes)
```

### 3.2 Key Types (Implemented)

-   `SlabPage`: owns the allocation (refcounted)
-   `SlabMut`: a view into a page (mutable, implements `IoBufMut`)
-   `Bytes`: immutable view after IO completes
-   `IoBytes`: zero-copy wrapper for `Bytes` implementing `IoBuf` (write path)

This design specifically ensures **ecosystem-compatibility with `bytes`** while satisfying `io_uring` pointer stability.

### 3.3 IoBytes: Zero-Copy Write Wrapper

**Status**: ✅ Implemented in `monocoque-core/src/alloc.rs`

The `IoBytes` wrapper solves a critical integration point:

```rust
pub struct IoBytes(Bytes);

unsafe impl IoBuf for IoBytes {
    fn as_buf_ptr(&self) -> *const u8 { self.0.as_ptr() }
    fn buf_len(&self) -> usize { self.0.len() }
    fn buf_capacity(&self) -> usize { self.0.len() }
}
```

**Why this is safe**:

-   `Bytes` is immutable and refcounted
-   Pointer is stable (allocation never moves)
-   No mutable aliasing possible
-   Kernel only reads during write operations

This eliminates the `.to_vec()` memcpy that would otherwise occur on every write.

---

## 4. Memory Safety Invariants (Non-Negotiable)

### Invariant A — Pointer Stability

**While a buffer is in-flight in the kernel, its pointer must not move.**

Guaranteed by:

-   backing allocation stored in an owning object (`Arc<SlabPage>` or equivalent)
-   `SlabMut` only ever stores a `NonNull<u8>` pointing into that allocation
-   the allocation is never reallocated/moved (no Vec growth, no Box swap)

**Forbidden:**

-   storing data in a `Vec<u8>` that might reallocate
-   exposing a mutable slice that allows resizing

### Invariant B — Exclusive Mutable Access

**At most one mutable view exists over the same memory region at a time.**

Guaranteed by:

-   ownership-passing: `SlabMut` is moved into async IO call
-   not cloneable
-   `IoBufMut` exposes pointer/len but does not allow aliasing

### Invariant C — Correct Initialization Tracking

**The kernel may write fewer bytes than requested. Uninitialized tail must never be visible.**

Guaranteed by:

-   `SetBufInit` implementation (or equivalent internal tracking)
-   `freeze(n)` only exposes `n` initialized bytes in returned `Bytes`

### Invariant D — No Mutation After Freeze

**Once converted to `Bytes`, the region is immutable.**

Guaranteed by:

-   `Bytes` only provides shared immutable access
-   `SlabMut` is consumed by `freeze`
-   the underlying page is not reused until refcount drops (or reuse is gated)

---

## 5. IoBuf / IoBufMut “Pre-Flight” Checklist

This is the critical correctness surface.

### 5.1 What `compio` Assumes

When `compio` takes an `IoBufMut` into an `io_uring` operation, it assumes:

1. `as_mut_ptr()` is valid for `len()` bytes
2. pointer remains valid until buffer is returned
3. it may write up to `len()` bytes
4. bytes beyond `set_init(n)` are uninitialized and must never be read

### 5.2 Minimal Correct Trait Behavior

The implementation must ensure:

-   `stable_ptr`: pointer refers to stable allocation
-   `capacity`: `len()` equals writable capacity
-   `init`: after completion, init is updated to actual bytes read
-   `freeze(n)` creates `Bytes` that only covers initialized region

---

## 6. The Top 5 Footguns (and How We Avoid Them)

### Footgun 1 — Partial Write/Read Assumptions

-   Fixed by `flush_vectored_all` advancing slices
-   Read path uses decoder that can handle split frames

### Footgun 2 — Dangling Pointer via Owner Drop

-   Fixed by `Arc` owner captured inside `SlabMut`
-   Owner lives until `Bytes` drops

### Footgun 3 — Exposing Uninitialized Memory

-   Fixed by strict `SetBufInit` + `freeze(n)` bounded view

### Footgun 4 — Mutable Alias (UB)

-   Fixed by move-only `SlabMut` and ownership-passing IO API

### Footgun 5 — Reuse While Still Referenced (Use-after-free)

-   Fixed by refcount behavior of `Bytes` owner or arena “graveyard”
-   Reuse only allowed when strong count indicates no outstanding views

---

## 7. Testing the Unsafe Boundary

### 7.1 Miri / Sanitizers Strategy

Because this project touches async IO + unsafe, the testing strategy is layered:

1. **Unit tests (safe, deterministic)**

    - `freeze` bounds
    - init tracking
    - slice correctness

2. **Interop tests with libzmq**

    - validates handshake correctness (no silent hangs)
    - validates real kernel writes into your buffers

3. **Stress tests**

    - many connections with random disconnects
    - verify no crashes, no memory corruption

4. **Sanitizers (recommended)**

    - AddressSanitizer for use-after-free
    - ThreadSanitizer for races (mostly should be none due to ownership)

_(Miri may not love io_uring runtime paths, but unit tests for slab logic should be miri-friendly.)_

---

## 8. What “Safe Enough” Looks Like

You are safe if:

-   all `unsafe` blocks are small and documented
-   invariants are enforced structurally (move-only types)
-   `freeze` is the only path to share data
-   no raw pointer escapes beyond the buffer traits
-   all protocols operate on `Bytes` and do not touch memory directly
