# Changelog

## Unreleased

- Fix: Synchronous ZMTP handshake performed before spawning IO/integration tasks to eliminate handshake races for REQ/REP/DEALER/ROUTER.
- Perf: Zero-copy framing on send path — frame headers are encoded separately and bodies are sent without memcpy (header+body interleaved), eliminating payload copies during normal data path.
- Fix: Replaced copy-based `encode_frame` usage with `encode_frame_header` + interleaved bodies; retained `encode_frame` for small protocol commands.
- Fix: Handshake uses stack buffers for fixed-size elements and a bounded allocation for READY body (one-time per connection).
- Change: Added `ZmtpSession::new_active` and `ZmtpIntegratedActor::new_active` to create actors post-handshake.
- Change: Dealer and Router now perform handshake synchronously and use `new_active` to avoid races.
- Add: REQ/REP socket implementations (REQ/REP modules) with proper handshake integration and state machines for strict alternation.
- Add: Interop examples for REQ/REP with libzmq and a simple REQ/REP demo; updated request_reply example to use randomized ports.
- Docs: Updated progress and analysis docs to reflect implementation and interop test results.

### Notes

- All doc-tests pass. Integration/interop tests were executed in the development environment and validated against libzmq where applicable.
- Next recommended steps: open a PR, run CI, and optionally add a vectored-write syscall path to submit header+body in a single ownership-passing operation.
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

-   Initial implementation of `monocoque` messaging runtime
-   Core messaging kernel with zero-copy semantics
-   ZMTP 3.1 protocol implementation
    -   DEALER socket (async request-reply client)
    -   ROUTER socket (identity-based routing server)
    -   PUB socket (publisher for broadcast)
    -   SUB socket (subscriber with topic filtering)
-   NULL mechanism authentication
-   Identity-based routing with epoch-based ghost-peer prevention
-   Topic-based pub/sub with sorted prefix table matching
-   Split-pump I/O architecture for cancellation safety
-   `io_uring`-based async I/O via `compio`
-   Zero-copy message handling with `bytes::Bytes`
-   Feature-gated protocol support (`zmq` feature)
-   Comprehensive blueprint documentation
-   Interoperability examples for testing with libzmq
    -   `interop_dealer_libzmq.rs` - DEALER ↔ libzmq ROUTER
    -   `interop_router_libzmq.rs` - ROUTER ↔ libzmq DEALER
    -   `interop_pubsub_libzmq.rs` - PUB ↔ libzmq SUB
-   Interoperability testing documentation (`docs/INTEROP_TESTING.md`)
-   Automated test runner script (`scripts/run_interop_tests.sh`)

### Fixed

-   **Critical**: Fixed handshake timing race condition in DEALER, ROUTER, and PUB sockets
    -   Issue: SocketActor spawned without initialization delay, causing greeting send/receive race
    -   Symptom: libzmq would close connection immediately after greeting exchange
    -   Solution: Ensured greeting is queued before SocketActor spawn, added 1ms delay for pump initialization
    -   Note: The 1ms delay is a pragmatic workaround; proper solution would use synchronization primitive (e.g., oneshot channel signal when pumps are ready)
    -   Impact: All socket types now successfully complete ZMTP handshake with libzmq
-   Fixed channel wiring in PUB socket
    -   Issue: `send()` was writing to disconnected channel (`app_tx_for_user` instead of `user_tx`)
    -   Symptom: `SendError` with BrokenPipe when publishing messages
    -   Solution: Corrected channel assignment and added task handle retention
    -   Impact: PUB socket can now send messages to subscribers
-   Fixed PUB socket task lifecycle management
    -   Issue: Integration task handle was dropped immediately after spawn
    -   Symptom: Task would abort before processing any messages
    -   Solution: Added `_task_handles` field to PubSocket struct
    -   Impact: PUB task now runs for the lifetime of the socket
-   Fixed PUB socket session event handling
    -   Issue: Used incorrect `on_bytes()` method instead of `session.on_bytes()`
    -   Symptom: Handshake would not complete properly
    -   Solution: Updated to use session-based event processing like DEALER/ROUTER
    -   Impact: PUB socket now correctly handles ZMTP handshake

### API

-   Public ergonomic socket types in `monocoque::zmq` module
-   Async/await API with `connect()`, `bind()`, `send()`, `recv()`
-   Prelude module for convenient imports
-   Full rustdoc documentation with examples
-   Comprehensive error documentation with `thiserror`

### Architecture

-   `monocoque-core`: Protocol-agnostic kernel (actors, hubs, allocator)
-   `monocoque-zmtp`: ZMTP 3.1 state machines
-   `monocoque`: Public API facade with feature gates

### Safety

-   Unsafe code isolated to `monocoque-core/src/alloc.rs` only
-   All protocol layers are 100% safe Rust
-   Formal invariants documented in blueprints
-   `#[deny(unsafe_code)]` enforced at crate level

### Performance

-   Zero-copy writes with `IoBytes` wrapper (eliminates `.to_vec()` memcpy)
-   Zero-copy fanout for PUB/SUB (refcount-based `Bytes::clone()`)
-   Ownership-passing I/O for kernel safety
-   Split-pump architecture (independent read/write paths)
-   Lock-free SPSC channels via `flume`
-   Cache-friendly sorted prefix table for subscriptions

### Documentation

-   Complete Cargo.toml metadata for all crates
-   CHANGELOG.md following Keep a Changelog format
-   PUBLISHING.md with crates.io publication guide
-   11 working examples demonstrating socket patterns
-   3 interoperability examples for libzmq compatibility testing
-   Blueprint documentation covering design decisions
-   API guidelines compliance (`# Errors` sections, `#[must_use]` annotations)
-   Timeless documentation (no hardcoded dates)

### Changed

-   **Refactored**: Split `monocoque/src/zmq/mod.rs` into separate files per socket type
    -   Extracted common error conversion helper to `common.rs`
    -   Split DealerSocket into `dealer.rs` (~140 lines)
    -   Split RouterSocket into `router.rs` (~155 lines)
    -   Split PubSocket into `publisher.rs` (~70 lines)
    -   Split SubSocket into `subscriber.rs` (~90 lines)
    -   Updated `mod.rs` to module re-exports and documentation (~60 lines)
    -   Impact: Improved code organization, easier maintenance, reduced cognitive load (60-155 lines per file vs 450 lines monolithic file)
    -   All public APIs remain unchanged, backward compatible
    -   All interop tests passing

### Testing

-   Integration tests with libzmq interoperability
-   Standalone interop examples for manual verification
-   Doctests for all public APIs

### Fixed

-   Unused variable warnings in `dealer.rs` and `router.rs`
-   Unit tests for core components
-   All tests passing with zero errors

[Unreleased]: https://github.com/vorjdux/monocoque/commits/main
