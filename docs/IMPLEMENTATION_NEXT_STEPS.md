# Next Steps Implementation Guide

This document tracks the immediate next actions for Monocoque development.

## Phase 1: Verify Interoperability (IN PROGRESS)

### Status: ✅ Interop Examples Created

Three standalone examples have been created for manual verification:

1. **DEALER ↔ libzmq ROUTER** (`examples/interop_dealer_libzmq.rs`)
2. **ROUTER ↔ libzmq DEALER** (`examples/interop_router_libzmq.rs`)
3. **PUB ↔ libzmq SUB** (`examples/interop_pubsub_libzmq.rs`)

### ⏭️ Next Action: Run Interop Examples

```bash
# Install libzmq first
sudo apt install libzmq3-dev  # Ubuntu/Debian
# or
brew install zeromq           # macOS

# Run each example
cargo run --example interop_dealer_libzmq --features zmq
cargo run --example interop_router_libzmq --features zmq
cargo run --example interop_pubsub_libzmq --features zmq
```

**Expected Result**: All three examples should complete successfully with "✅" output.

**If They Fail**: Debug the handshake/protocol issues. Common problems:

-   READY command metadata incorrect
-   Frame encoding (short vs long frames)
-   Identity envelope format
-   Greeting version negotiation

### Documentation

See `docs/INTEROP_TESTING.md` for detailed instructions.

---

## Phase 2: Fix Automated Tests

### Issue

Integration tests in `monocoque/tests/` hang due to compio runtime lifecycle issues:

```rust
#[test]
#[ignore = "compio runtime lifecycle issues in test harness"]
fn test_interop_pair() { /* ... */ }
```

### Solution Options

1. **Convert to criterion benchmarks** (better runtime control)
2. **Use `tokio::test` macro** (if acceptable to depend on tokio for tests)
3. **Create test-specific runtime wrapper** that handles cleanup properly
4. **Keep as ignored tests** and rely on examples for verification

### ⏭️ Next Action After Interop Works

Choose solution and implement automated test infrastructure.

---

## Phase 3: Multi-Peer Integration Tests

Once single-peer interop is verified, add tests for:

### ROUTER Load Balancing

```rust
// Test: 3 DEALER clients → 1 ROUTER
// Verify: Round-robin distribution
// Verify: Identity routing works
// Verify: Ghost peer cleanup on disconnect
```

### PUB/SUB Fanout

```rust
// Test: 1 PUB → 3 SUB clients
// Verify: Overlapping subscriptions (e.g., "A", "AB", "ABC")
// Verify: Deduplication works correctly
// Verify: Unsubscribe removes peer from fanout
```

### Stress Testing

```rust
// Test: High message rate (1000 msg/sec)
// Test: Random disconnects
// Test: Reconnection with identity changes
// Verify: No panics, no memory leaks, no dropped messages
```

---

## Phase 4: Error Handling Improvements

Current gaps:

1. Too many `unwrap()` calls in hot paths
2. No timeout handling (handshake, read, write)
3. No graceful shutdown sequence
4. No backpressure implementation (BytePermits is NoOp)

### ⏭️ Actions

1. Define `MonocoqueError` enum with proper error types
2. Add `Result<T, MonocoqueError>` to fallible operations
3. Implement timeouts using `compio::time::timeout()`
4. Add graceful shutdown logic (drain queues, send goodbye frames)

---

## Phase 5: Documentation Pass

### Current State

-   ✅ Blueprint documentation (comprehensive)
-   ✅ CHANGELOG.md (Keep a Changelog format)
-   ✅ Examples (11 total, including 3 interop)
-   ⚠️ API docs (minimal rustdoc coverage)

### ⏭️ Actions

1. Add rustdoc to all public APIs
2. Add `/// # Examples` sections
3. Generate and review `cargo doc` output
4. Write `GETTING_STARTED.md` tutorial
5. Add architecture diagram to README

---

## Phase 6: Performance Benchmarking

After correctness is proven, benchmark against libzmq:

### Metrics to Measure

1. **Latency**: Round-trip time (DEALER-ROUTER-DEALER)
2. **Throughput**: Messages per second
3. **Memory**: Heap allocations, RSS
4. **CPU**: Cycles per message

### Tools

```bash
# Use criterion for benchmarks
cargo bench --features zmq

# Use perf for profiling
perf record -g cargo run --release --example high_throughput
perf report
```

---

## Priority Order (Recommended)

1. 🔴 **Critical**: Verify interop examples work (Phase 1)
2. 🟡 **High**: Fix automated tests (Phase 2)
3. 🟡 **High**: Multi-peer integration tests (Phase 3)
4. 🟢 **Medium**: Error handling improvements (Phase 4)
5. 🔵 **Low**: Documentation pass (Phase 5)
6. 🔵 **Low**: Performance benchmarking (Phase 6)

---

## Current Blockers

None! All code compiles, examples are ready to run.

**Next immediate step**: Install libzmq and run the three interop examples.

---

## Recent Changes

-   ✅ Created 3 interop examples with libzmq
-   ✅ Fixed unused variable warnings
-   ✅ Updated CHANGELOG.md
-   ✅ Created docs/INTEROP_TESTING.md
-   ✅ Removed all temporal references from documentation
-   ✅ Updated implementation status to reflect Phase 2-3 complete

Last updated by: GitHub Copilot (automated documentation sync)
