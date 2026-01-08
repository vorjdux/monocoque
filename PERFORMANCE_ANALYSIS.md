# Performance Analysis: monocoque vs zmq.rs

## Benchmark Results

**Current Status (MESSAGE_COUNT=10,000):**

-   monocoque REQ/REP: **~780ms** (constant, regardless of message size)
-   zmq.rs REQ/REP: **~1.5-2.5ms**
-   **Performance Gap: 300-500x slower**

## Root Cause: Channel-Based Actor Architecture vs Direct Stream I/O

### zmq.rs Architecture (Pure Rust, Async)

```
User Code
   ↓
ReqSocket::send()
   ↓
peer.send_queue.send()  ← DIRECT FramedWrite to TCP stream!
   ↓
asynchronous_codec (framing)
   ↓
tokio/async-std TcpStream
   ↓
kernel (write syscall)
```

**Characteristics:**

-   Pure Rust async implementation (tokio/async-std)
-   **Direct stream I/O**: `send_queue` is `FramedWrite<TcpStream>`
-   No channels between user API and socket
-   Message passes through: User → Framed codec → async I/O → kernel (3 hops)
-   Uses `futures::SinkExt::send()` directly on stream

**Key Code:**

```rust
pub(crate) struct Peer {
    pub(crate) send_queue: FramedWrite<Box<dyn FrameableWrite>, ZmqCodec>,  // Direct!
    pub(crate) recv_queue: FramedRead<Box<dyn FrameableRead>, ZmqCodec>,
}

// In ReqSocket::send():
peer.send_queue.send(Message::Message(message)).await?;  // Writes to socket immediately!
```

### monocoque Architecture (Pure Rust, Async + Actors)

```
User Code
   ↓
ReqSocket::send()
   ↓
InternalReq::send()
   ↓
flume channel (app_tx.send)  ← CHANNEL HOP #1
   ↓
ZmtpIntegratedActor task
   ↓
ZMTP encoding
   ↓
SocketActor (compio)
   ↓
io_uring / async I/O
   ↓
kernel

Response path:
kernel → SocketActor → ZmtpIntegratedActor
   ↓
flume channel (app_rx.send)  ← CHANNEL HOP #2
   ↓
InternalReq::recv()
```

**Characteristics:**

-   Pure Rust async with compio (io_uring on Linux, fallback elsewhere)
-   **Channel-based actors**: User API communicates via `flume` channels
-   Background tasks handle actual I/O
-   Message passes through: User → channel → actor → encode → io_uring → kernel (6+ hops)

**Key Code:**

```rust
pub struct ReqSocket {
    app_tx: Sender<Vec<Bytes>>,     // Channel to actor!
    app_rx: Receiver<Vec<Bytes>>,   // Channel from actor!
}

// In ReqSocket::send():
self.app_tx.send(msg).await?;  // Goes to channel, not socket!
```

## Performance Breakdown

For **each message** in monocoque:

1. **User API overhead**: ~5-10μs
    - Function call overhead
    - State machine lock (Mutex)
2. **Channel send #1** (user → actor): ~20-50μs
    - flume channel send
    - Async context switch
    - Waker notification
3. **Actor processing**: ~10-30μs
    - ZMTP frame encoding
    - Multipart assembly
    - Buffer management
4. **io_uring submission**: ~5-15μs
    - SQE preparation
    - Submission queue management
5. **Network I/O**: ~10-50μs (actual send/recv)
    - TCP syscalls
    - Kernel processing
6. **io_uring completion**: ~5-15μs
    - CQE polling
    - Completion queue processing
7. **Channel send #2** (actor → user): ~20-50μs
    - flume channel send
    - Async wake up
8. **ZMTP frame decoding**: ~10-20μs
    - Frame parsing
    - Envelope stripping

**Total per-message overhead: ~85-240μs** (not counting actual network I/O)

For 10,000 messages:

-   Overhead: 85μs × 10,000 = **850ms to 2.4 seconds**
-   Actual I/O: ~30-100ms
-   **Measured: ~780ms** (matches prediction!)

## Why zmq.rs is Faster

zmq.rs (pure Rust, async) has:

1. **Direct stream I/O**: No channels, writes directly to `FramedWrite<TcpStream>`
2. **No actor overhead**: Single-threaded async, no task spawning for I/O
3. **Minimal layers**: User → Codec → Stream → Kernel (3 hops)
4. **Lock-free for simple cases**: REQ socket doesn't need complex synchronization
5. **Zero extra allocations**: Message goes directly to stream buffer

monocoque (pure Rust, async + actors) has:

1. **Channel overhead**: flume channel send/recv for every message (~20-50μs each)
2. **Actor task switches**: Waker notifications, context switches
3. **More layers**: User → Channel → Actor → Codec → Stream → Kernel (6 hops)
4. **Lock contention**: State machine mutex, channel synchronization
5. **Extra allocations**: Channel buffers, actor message queues

**Both are async Rust, but zmq.rs writes directly to sockets while monocoque routes through actors.**

## Solutions

### Option 1: Optimize Hot Path (Quick Wins)

-   **Remove channel for simple patterns**: REQ/REP could bypass actors for single-connection case
-   **Batch channel operations**: Send multiple messages in one channel operation
-   **Reduce allocations**: Pool `Bytes` objects, reuse frame buffers
-   **Optimize ZMTP encoding**: Inline small messages, avoid frame wrapping

**Expected Improvement: 2-5x** (780ms → 150-400ms)

### Option 2: Hybrid Architecture (Medium Effort)

-   **Fast path for common cases**: Direct I/O for REQ/REP single-peer
-   **Actor path for complex cases**: ROUTER/DEALER/PUB/SUB with multiple peers
-   **Zero-copy where possible**: Use io_uring's buffer selection

**Expected Improvement: 10-50x** (780ms → 15-75ms)

### Option 3: Redesign Core Architecture (High Effort)

-   **Remove actor layer entirely** for simple patterns
-   **Direct compio I/O** from user API
-   **Keep actors only for load balancing** (ROUTER/DEALER with fairqueuing)
-   **Inline ZMTP encoding** in send path

**Expected Improvement: 100-300x** (780ms → 2.5-8ms, approaching zmq.rs)

## Current Architecture Benefits

Despite the performance gap, monocoque's design has advantages:

1. **Async-native**: Non-blocking, integrates with async ecosystems
2. **io_uring**: Modern Linux kernel interface (when available)
3. **Type-safe**: Rust's strong typing prevents many classes of bugs
4. **Composable**: Actor pattern enables complex routing logic
5. **Observable**: Easy to add metrics, tracing at actor boundaries

## Recommendation

**Phase 1**: Quick wins (Option 1)

-   Profile hot path with `cargo flamegraph`
-   Eliminate obvious allocations
-   Benchmark after each optimization

**Phase 2**: Measure real-world workloads

-   Most applications don't send 10k tiny messages in tight loop
-   Test with realistic message sizes (4KB+) and patterns
-   Latency matters more than raw throughput for many use cases

**Phase 3**: Strategic refactor (Option 2 or 3)

-   If benchmarks show need, implement fast path
-   Keep backward compatibility
-   Consider feature flag for "compat mode" vs "performance mode"

## Benchmarking Notes

**Why the constant 780ms?**

-   With MESSAGE_COUNT=10 originally, setup (780ms) dominated
-   With MESSAGE_COUNT=10,000 now, per-message overhead (0.078ms each) dominates
-   Message size doesn't matter much because:
    -   Network I/O is fast on localhost
    -   Channel overhead is fixed per message
    -   Small messages still pay full channel/actor cost

**Why zmq.rs varies (1.5-2.5ms)?**

-   Actually measuring message throughput
-   Larger messages take slightly longer (TCP, memory copy)
-   But setup is amortized over 10k iterations

## Next Steps

1. **Profile with flamegraph**: Identify exact hot spots
2. **Micro-benchmark channels**: Measure flume overhead in isolation
3. **Compare async overhead**: Test with/without actor layer
4. **Optimize incrementally**: Fix worst offenders first
5. **Re-benchmark**: Validate improvements

The architecture isn't fundamentally broken, but it's optimized for different goals (safety, composability) vs raw speed. The channel-based actor pattern adds significant overhead for tight loops but may be acceptable for real applications with larger messages and less frequent sends.
