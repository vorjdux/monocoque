# Phase 0: Memory Management and Direct I/O

**Status**: ✅ **COMPLETE**

## Overview

Phase 0 provides the foundation for safe, zero-copy I/O operations in Monocoque:

-   `IoArena` and `SlabMut` for efficient buffer allocation
-   `IoBytes` for zero-copy writes
-   `SegmentedBuffer` for receive buffering
-   Direct stream I/O pattern in all sockets

---

## 1. Memory Management

### IoArena

Thread-local buffer pool that provides 4KB slabs for network operations:

```rust
pub struct IoArena {
    pool: Vec<BytesMut>,
    capacity: usize,
}

impl IoArena {
    pub fn alloc(&mut self) -> SlabMut {
        // Returns 4KB buffer, either from pool or newly allocated
    }
}
```

**Key Properties:**

-   Fixed 4KB slabs (matches typical MTU)
-   Reuses buffers without syscalls
-   No synchronization needed (thread-local)

### SlabMut

Wrapper around `BytesMut` that enforces ownership discipline:

```rust
pub struct SlabMut(BytesMut);

impl SlabMut {
    pub fn freeze(self, len: usize) -> Bytes {
        // Converts to immutable Bytes at exact length
    }
}
```

**Safety Invariants:**

-   Must be moved to kernel for read operations
-   Returned after read completes
-   Converted to `Bytes` after freezing

### IoBytes

Zero-copy wrapper for write operations:

```rust
pub struct IoBytes(Bytes);

impl IoBuf for IoBytes {
    // Kernel reads from this buffer during writes
}
```

**Purpose:**

-   Satisfies compio's ownership requirements
-   Allows kernel to access buffer during async write
-   No intermediate copies

### SegmentedBuffer

Accumulates received data for protocol decoding:

```rust
pub struct SegmentedBuffer {
    segments: VecDeque<Bytes>,
    total_len: usize,
}

impl SegmentedBuffer {
    pub fn extend(&mut self, bytes: Bytes) {
        // Adds new segment
    }

    pub fn try_get_bytes(&mut self, len: usize) -> Option<Bytes> {
        // Returns bytes if available, handles segmentation
    }
}
```

**Features:**

-   Handles data spanning multiple reads
-   Zero-copy segment management
-   Efficient for protocol frame extraction

---

## 2. Direct Stream I/O Pattern

Each socket owns its stream directly and performs all I/O inline:

```rust
pub struct DealerSocket<S = TcpStream>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream: S,
    decoder: ZmtpDecoder,
    arena: IoArena,
    recv: SegmentedBuffer,
    write_buf: BytesMut,
}
```

### Read Path

```rust
pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
    loop {
        // Try to decode from existing buffer
        if let Some(msg) = self.decoder.try_decode(&mut self.recv)? {
            return Ok(Some(msg));
        }

        // Need more data - read from stream
        let slab = self.arena.alloc();
        let (n, slab) = self.stream.read(slab).await;
        let bytes = slab.freeze(n?);
        self.recv.extend(bytes);
    }
}
```

**Flow:**

1. Try to decode from buffered data
2. If need more, allocate slab from arena
3. Read from stream (kernel writes to slab)
4. Freeze slab to `Bytes`
5. Add to receive buffer
6. Repeat until complete message decoded

### Write Path

```rust
pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
    self.write_buf.clear();
    encode_multipart(&mut self.write_buf, &msg);

    let iobytes = IoBytes::new(self.write_buf.clone().freeze());
    self.stream.write_all(iobytes).await?;
    Ok(())
}
```

**Flow:**

1. Encode message frames to buffer
2. Wrap in `IoBytes` for ownership transfer
3. Write to stream (kernel reads from buffer)
4. Await completion

---

## 3. Design Benefits

### Simplicity

-   Direct async/await flow
-   Clear ownership at every step
-   No coordination or intermediate tasks

### Performance

-   Zero-copy from kernel to `Bytes`
-   Buffer reuse via `IoArena`
-   No allocator pressure

### Safety

-   Ownership enforced at type level
-   No shared mutable state
-   Cancellation-safe (no poisoned locks)

### Flexibility

-   Generic over `AsyncRead + AsyncWrite`
-   Easy to customize per-socket
-   No hidden complexity

---

## 4. Partial Write Handling

Non-blocking writes may complete partially. The compio runtime handles this correctly:

```rust
// Internal behavior:
// 1. Submit write batch
// 2. Kernel reports n bytes written
// 3. Advance cursor:
//    - Fully written buffers → drop
//    - Partially written → slice remaining
// 4. Retry with remaining data
```

This avoids:

-   Data duplication
-   Data loss
-   Infinite loops

---

## 5. What Phase 0 Enables

With this foundation, higher layers can:

1. **Perform I/O without memory safety concerns**

    - Ownership enforced by types
    - No manual buffer management

2. **Pass messages as `Bytes` without copying**

    - Zero-copy all the way
    - Efficient message forwarding (ROUTER → DEALER)

3. **Focus on protocol logic**

    - Don't worry about kernel interaction
    - Arena handles buffer lifecycle

4. **Maintain performance**
    - No hidden allocations
    - Predictable latency

---

## 6. Implementation Status

✅ All components complete and tested:

-   `IoArena` and `SlabMut` allocator
-   `IoBytes` zero-copy wrapper
-   `SegmentedBuffer` receive buffering
-   Direct I/O in all 6 socket types
-   Integration tests validating correctness
-   All safety invariants enforced

This foundation makes everything else possible.
