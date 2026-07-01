# Architecture Decision Records

Three one-page ADRs for the most consequential design choices in Monocoque.

---

## ADR-1: io_uring / compio as the default runtime

**Status**: Accepted (compio is the default); updated 2026-Q2 to add an optional tokio backend  
**Date**: 2025-Q4

### Context

Monocoque requires sub-30 μs round-trip latency and >1 M msg/sec throughput while staying entirely in safe Rust. Two mainstream async runtimes were evaluated: Tokio (epoll-based) and compio (io_uring-based).

### Decision

Use **compio** as the default async runtime. compio exposes io_uring's submission-queue/completion-queue model through Rust futures, eliminating the `epoll → userspace → syscall` round-trip present in Tokio's reactor.

The socket stack is generic over the `compio::io` `AsyncRead`/`AsyncWrite` traits and never names a runtime directly, so a second backend slots in behind the same abstraction. A tokio backend now ships behind the `runtime-tokio` feature for platforms without io_uring; it follows the same thread-per-core model (current-thread runtime, no work stealing). compio stays the default. io_uring's design win (no context switches, no per-recv syscall) is aimed at real network I/O and high connection counts; on single-flow loopback microbenchmarks the epoll backend can actually edge it, so treat the two as workload-dependent rather than one being strictly faster (see docs/performance.md).

### Consequences

| Trait | Tokio (epoll) | compio (io_uring) |
|-------|-------------|------------------|
| Context switches per I/O | 2 (ready + read) | 0 (completion in userspace) |
| Syscall per recv | 1 (`recv`) | 0 (batch via SQ) |
| Linux kernel requirement | 3.x+ | 5.1+ (5.11+ recommended) |
| Windows / macOS | ✅ | ❌ (io_uring is Linux-only) |
| Ecosystem size | Large | Small but growing |

**Trade-off accepted**: the default backend is Linux-only, chosen for io_uring's design advantages on real network I/O. Portability is covered by the tokio backend, which ships today behind the `runtime-tokio` feature (macOS/Windows) and rides the same `AsyncRead + AsyncWrite` abstraction, since the protocol layer is runtime-agnostic.

---

## ADR-2: Worker-pool per PubSocket instead of single-threaded select

**Status**: Accepted  
**Date**: 2025-Q4

### Context

`PubSocket` must fan out each published message to potentially thousands of concurrent TCP subscribers. Two approaches were evaluated:

- **Option A**: Single async task, `futures::select!` over all subscriber streams.
- **Option B**: Fixed worker pool  -  N OS threads each running their own compio runtime, each owning a shard of subscribers.

### Decision

Use **Option B** (worker pool, default worker count = CPU core count).

### Rationale

Option A (single-task select) hits a hard scalability wall: with K subscribers, each `send` must await K sequential write futures. Even with io_uring batching, the single thread becomes the bottleneck above ~200 subscribers.

Option B removes that bottleneck:

- Each worker thread owns ~(total_subscribers / N) connections and runs its own io_uring ring.
- A published message is wrapped in `Arc<Bytes>` (zero copy) and sent to each worker via a bounded `flume` channel.
- Workers write to their subscriber shards independently and in parallel.
- The send-HWM is enforced per-worker with `try_send`; backpressure drops messages rather than blocking the publisher.

**Trade-off accepted**: A fixed thread pool consumes N × (stack + runtime overhead) even with zero subscribers. For typical server deployments this is negligible; for embedded/constrained environments the worker count can be set to 1 via `PubSocket::bind_with_workers(addr, 1)`.

---

## ADR-3: Use `Bytes` (refcount slices) instead of copying message frames

**Status**: Accepted  
**Date**: 2025-Q4

### Context

Every multipart message travels through at least one channel (inbound queue, outbound worker channel, application layer). Naive approaches copy frame bytes at each boundary; at 3 M msg/sec this becomes the dominant allocation cost.

### Decision

Use **`bytes::Bytes`**  -  an atomically reference-counted, immutable byte slice  -  as the canonical frame type throughout the entire stack.

### Rationale

```
send(vec![Bytes::from("topic"), payload.clone()])
         ↑                       ↑
         O(1) clone (ref bump)   O(1) clone (ref bump)
```

- **Fan-out is free**: The PUB worker pool clones `Arc<Bytes>` to N workers  -  no heap allocation per worker.
- **recv is zero-copy**: The TCP reader fills a `BytesMut`, then calls `.freeze()` to get a `Bytes` without any copy.
- **No lifetime complexity**: `Bytes` is `'static`  -  it can cross thread and task boundaries without borrow-checker gymnastics.
- **`BytesMut` for mutable staging**: Protocol framing (ZMTP length prefix, flags) builds into `BytesMut` then freezes once complete.

**Trade-off accepted**: Reference counting adds a 1-2 ns atomic increment per clone. At our message rates this is unmeasurable next to the I/O cost. For single-subscriber paths where no fan-out occurs, a copy would be marginally cheaper per message but would require a different type throughout the API, increasing complexity.
