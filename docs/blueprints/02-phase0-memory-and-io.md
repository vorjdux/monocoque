# Phase 0: Memory Management and Direct I/O

**Status**: ✅ **COMPLETE**

## Overview

Phase 0 provides the foundation for safe, zero-copy I/O operations in Monocoque:

-   `core::io` read-slab helpers (`take_read_buffer`, `fill_read`) for efficient buffer allocation
-   Direct `Bytes` writes (compio's `bytes` feature) for zero-copy sends
-   `SegmentedBuffer` for receive buffering
-   Direct stream I/O pattern in all sockets

---

## 1. Memory Management

### Read slab (`core::io`)

The read path uses a reused `BytesMut` slab, not a pinned arena. A socket keeps
a single `BytesMut` field, grown lazily on the first read, and `take_read_buffer`
carves each read off it:

```rust
pub const READ_SLAB_SIZE: usize = 64 * 1024;

/// Carve a `read_size` buffer off the front of a reused stash, growing a fresh
/// `READ_SLAB_SIZE` slab when the tail runs out.
pub unsafe fn take_read_buffer(stash: &mut BytesMut, read_size: usize) -> BytesMut;
```

**Key Properties:**

-   Read buffer size is clamped to `READ_SLAB_SIZE` (64 KiB)
-   Successive reads carve `read_size` chunks off one slab until it is used up,
    then a fresh 64 KiB slab is allocated
-   Allocated lazily, so an idle socket holds no read buffer
-   A frozen buffer shares the slab allocation via `bytes` refcounting, so a
    lagging consumer pins the slab exactly as the old arena page did

### fill_read (`core::io`)

The one place in the workspace that calls `IoBufMut::set_buf_init`:

```rust
/// Read into an owned buffer's spare capacity, then declare the bytes written
/// as initialized. Every runtime backend routes its read through here.
pub async fn fill_read<B, F>(buf: B, read: F) -> BufResult<usize, B>
where
    B: IoBufMut,
    F: AsyncFnOnce(&mut [MaybeUninit<u8>]) -> io::Result<usize>;
```

**Safety Invariants:**

-   The buffer is moved into the read, then returned (ownership-passing)
-   `set_buf_init(n)` declares exactly the byte count the read reported
-   Callers `truncate` to the bytes actually read before freezing to `Bytes`

### Zero-copy writes

There is no write wrapper type. Compio's `bytes` feature implements `IoBuf` for
`Bytes` directly, so an encoded buffer is frozen to `Bytes` and passed straight
to `write_all`:

**Purpose:**

-   Satisfies compio's ownership requirements with no wrapper
-   Lets the kernel read the buffer during the async write
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
    read_buf: BytesMut, // reused read slab, grown lazily on first read
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

        // Need more data - carve a read buffer off the reused slab
        let buf = unsafe { take_read_buffer(&mut self.read_buf, read_size) };
        let (n, mut buf) = self.stream.read(buf).await;
        let n = n?;
        buf.truncate(n);
        self.recv.extend(buf.freeze());
    }
}
```

**Flow:**

1. Try to decode from buffered data
2. If need more, carve a read buffer off the reused slab (`take_read_buffer`)
3. Read from stream (kernel writes into the buffer via `fill_read`)
4. Truncate to the bytes read, then freeze to `Bytes`
5. Add to receive buffer
6. Repeat until complete message decoded

### Write Path

```rust
pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
    self.write_buf.clear();
    encode_multipart(&mut self.write_buf, &msg);

    let buf = self.write_buf.split().freeze();
    self.stream.write_all(buf).await?;
    Ok(())
}
```

**Flow:**

1. Encode message frames to buffer
2. Freeze to `Bytes` for ownership transfer (compio's `bytes` feature)
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
-   Buffer reuse via the reused read slab (`take_read_buffer`)
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
    - `core::io` handles buffer lifecycle

4. **Maintain performance**
    - No hidden allocations
    - Predictable latency

---

## 6. Implementation Status

✅ All components complete and tested:

-   `core::io` read-slab helpers (`take_read_buffer`, `fill_read`)
-   Direct `Bytes` zero-copy writes (compio's `bytes` feature)
-   `SegmentedBuffer` receive buffering
-   Direct I/O in all 6 socket types
-   Integration tests validating correctness
-   All safety invariants enforced

This foundation makes everything else possible.
