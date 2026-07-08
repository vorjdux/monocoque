# 📄 File 2 - `01-unsafe-boundary-and-allocator.md`

# Unsafe Boundary & Allocator

_How Monocoque uses `unsafe` without breaking Rust's guarantees_

---

## 1. Why Unsafe Exists Here (and Only Here)

Monocoque needs unsafe code for exactly one reason:

> `io_uring` requires that memory passed to the kernel **does not move** while an operation is in flight.

Rust can't express "this pointer stays stable while the kernel owns it" purely in the type system without either:

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

-   `monocoque-core/src/io.rs` (read-buffer helpers: `fill_read`, `take_read_buffer`)
-   `monocoque-core/src/tcp.rs` (transport glue)

Each opts back into `unsafe_code` that the crate otherwise denies, and each
keeps its `unsafe` behind a safe API.

All other crates/modules:

-   `monocoque-core/src/router/*`
-   `monocoque-core/src/pubsub/*`
-   `monocoque-zmtp/*`

must be safe Rust only.

---

## 3. The Read-Buffer Contract

### 3.1 Intended Semantics

The read path uses a reused `BytesMut` slab rather than a pinned arena. Two
helpers in `monocoque-core/src/io.rs` carry the whole contract:

-   `take_read_buffer` carves a read-sized buffer off the front of the slab
-   `fill_read` reads into that buffer's spare capacity and declares the bytes
    it read initialized

The buffer is owned by the read for the duration of the syscall, then
"freezes" into `Bytes` for zero-copy usage.

Conceptually:

```
BytesMut slab (reused, grown lazily to READ_SLAB_SIZE)
    |
    | take_read_buffer(stash, read_size)
    v
BytesMut buffer (owned, moved into the read via fill_read)
    |
    | truncate(n) + freeze()
    v
Bytes (immutable, shareable, refcounted; shares the slab allocation)
```

### 3.2 Key Types (Implemented)

-   `READ_SLAB_SIZE`: the 64 KiB read-buffer ceiling and minimum slab capacity
-   `take_read_buffer`: carves the next read buffer off the reused slab, growing
    a fresh `READ_SLAB_SIZE` slab when the tail runs out
-   `fill_read`: the one call site of `IoBufMut::set_buf_init`, shared by every
    runtime backend
-   `Bytes`: immutable view after the read completes, sharing the slab's
    allocation via `bytes` refcounting

This design specifically ensures **ecosystem-compatibility with `bytes`** while
satisfying `io_uring` pointer stability. Because the read buffer is grown lazily
on the first read, an idle socket holds no read buffer at all.

### 3.3 Zero-Copy Writes

The write path no longer needs a wrapper type. Compio's `bytes` feature
implements `IoBuf` for `Bytes` directly, so an encoded frame buffer is frozen to
`Bytes` and handed straight to `write_all` (or, on the vectored path, exposed as
borrowed `IoSlice`s by `core::io::with_vectored_slices`).

**Why this is safe**:

-   `Bytes` is immutable and refcounted
-   Pointer is stable (allocation never moves)
-   No mutable aliasing possible
-   Kernel only reads during write operations

This keeps the write path free of any `.to_vec()` memcpy.

---

## 4. Memory Safety Invariants (Non-Negotiable)

### Invariant A - Pointer Stability

**While a buffer is in-flight in the kernel, its pointer must not move.**

Guaranteed by:

-   the read buffer is a `BytesMut` carved off a reused slab; its allocation is
    not reallocated or moved while the read is in flight
-   ownership-passing hands the whole buffer to the read, so nothing resizes it
    mid-syscall
-   a fresh slab is only grown between reads, never during one

**Forbidden:**

-   growing the buffer (capacity change) while a read is in flight
-   exposing a mutable slice that allows resizing during IO

### Invariant B - Exclusive Mutable Access

**At most one mutable view exists over the same memory region at a time.**

Guaranteed by:

-   ownership-passing: the read buffer from `take_read_buffer` is moved into the
    read via `fill_read`
-   `fill_read` scopes the spare-capacity borrow to the read itself
-   `IoBufMut` exposes pointer/len but does not allow aliasing

### Invariant C - Correct Initialization Tracking

**The kernel may write fewer bytes than requested. Uninitialized tail must never be visible.**

Guaranteed by:

-   `fill_read` is the single caller of `IoBufMut::set_buf_init`, and declares
    exactly the byte count the read reported
-   callers `truncate` the buffer to the bytes actually read before freezing, so
    the returned `Bytes` covers only the initialized region

### Invariant D - No Mutation After Freeze

**Once converted to `Bytes`, the region is immutable.**

Guaranteed by:

-   `Bytes` only provides shared immutable access
-   the `BytesMut` buffer is consumed by `freeze`
-   the slab's allocation is not reused until its `bytes` refcount drops, so a
    lagging consumer pins the slab

---

## 5. IoBuf / IoBufMut "Pre-Flight" Checklist

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

### Footgun 1 - Partial Write/Read Assumptions

-   Fixed by `flush_vectored_all` advancing slices
-   Read path uses decoder that can handle split frames

### Footgun 2 - Dangling Pointer via Owner Drop

-   Fixed by `bytes` refcounting: a frozen buffer shares the slab allocation
-   The slab lives until the last `Bytes` view drops

### Footgun 3 - Exposing Uninitialized Memory

-   Fixed by `fill_read`'s single `set_buf_init` (exact read count) plus caller
    `truncate` before freeze

### Footgun 4 - Mutable Alias (UB)

-   Fixed by moving the owned `BytesMut` buffer into `fill_read` and the
    ownership-passing IO API

### Footgun 5 - Reuse While Still Referenced (Use-after-free)

-   Fixed by `bytes` refcounting on the slab allocation
-   A fresh slab is grown only when the current one has no room left, and a
    frozen buffer keeps its slab alive as long as any view outstands

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

## 8. What "Safe Enough" Looks Like

You are safe if:

-   all `unsafe` blocks are small and documented
-   invariants are enforced structurally (move-only types)
-   `freeze` is the only path to share data
-   no raw pointer escapes beyond the buffer traits
-   all protocols operate on `Bytes` and do not touch memory directly
