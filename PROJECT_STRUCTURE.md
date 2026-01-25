# Monocoque Project Organization

## Overview

This document describes the clean project structure after reorganization. The project follows a strict separation between **public** and **internal** crates.

## Crate Structure

### 🔓 monocoque (Public Crate)
**The only crate users should import**

```toml
[dependencies]
monocoque = { version = "0.1", features = ["zmq"] }
```

Contains:
- `src/lib.rs` - Public API exports
- `src/zmq/` - High-level socket API (DealerSocket, RouterSocket, etc.)
- `examples/` - 50+ examples demonstrating all features
- `benches/` - Performance benchmarks (throughput, latency, patterns)
- `tests/` - Integration tests

### 🔒 monocoque-core (Internal Crate)
Protocol-agnostic building blocks. **Not published to crates.io**.

**Protection**: `publish = false` in Cargo.toml prevents publishing.

Contains:
- `Message` - Zero-copy message type
- `SocketOptions` - Socket configuration
- `SocketType` - Socket type enum
- Core utilities used by protocol implementations

### 🔒 monocoque-zmtp (Internal Crate)
ZMTP 3.1 protocol implementation. **Not published to crates.io**.

**Protection**: `publish = false` in Cargo.toml prevents publishing.

Contains:
- Socket implementations (REQ, REP, DEALER, ROUTER, PUB, SUB, XPUB, XSUB)
- ZMTP handshake logic
- Frame codec (encoding/decoding)
- Security mechanisms (NULL, PLAIN, CURVE)

## Directory Structure

```
monocoque/
├── monocoque/           # Public crate - all user-facing code
│   ├── examples/        # All examples (moved from root)
│   ├── benches/         # All benchmarks (consolidated here)
│   │   └── interop/     # Interop benchmarks with libzmq
│   └── tests/           # Integration tests
│
├── monocoque-core/      # Internal - protocol-agnostic primitives
├── monocoque-zmtp/      # Internal - ZMTP implementation
│
├── docs/                # Documentation
│   ├── GETTING_STARTED.md
│   ├── USER_GUIDE.md
│   ├── SECURITY_GUIDE.md
│   ├── blueprints/      # Design documents
│   └── internal/        # Development/implementation docs
│
├── scripts/             # Centralized development scripts
│   ├── bench_all.sh
│   └── run_interop_tests.sh
│
├── interop_tests/       # Interop test suite (Python + Rust)
│   ├── test_req_rep_interop.py
│   ├── test_pub_sub_interop.py
│   ├── test_interop.sh
│   └── run_all_tests.sh
│
├── monocoque-fuzz/      # Fuzzing targets (cargo-fuzz)
│   ├── fuzz_targets/
│   │   └── fuzz_decoder.rs
│   ├── artifacts/       # Crash test cases (kept in repo)
│   └── corpus/          # Fuzzing corpus (kept in repo)
│
└── tests/               # Workspace-level integration tests
```

## What Was Cleaned Up

### ✅ Moved to Public Crate (monocoque/)
- **Benchmarks**: monocoque-zmtp/benches/ → monocoque/benches/
  - `performance.rs`, `measure_latency.rs`, `simple_perf.rs`
- **Examples**: root examples/ → monocoque/examples/
  - All 50+ example files now in public crate
- **Interop benchmarks**: benchmarks/ → monocoque/benches/interop/
  - `libzmq_throughput.py`

### ✅ Removed Duplicate Scripts
- ❌ `analyze_benchmarks.sh` (root) - duplicate
- ❌ `monocoque/quick_bench.sh` - duplicate
- ❌ `monocoque/run_benchmarks.sh` - duplicate
- ❌ `monocoque/analyze_benchmarks.py` - duplicate
- ✅ Kept: `scripts/bench_all.sh` and `scripts/run_interop_tests.sh`

### ✅ Organized Documentation
- **User docs** remain in `docs/`:
  - GETTING_STARTED, USER_GUIDE, SECURITY_GUIDE
  - COMPATIBILITY, PERFORMANCE, PRODUCTION_DEPLOYMENT
  - MIGRATION, PUBLISHING, FUZZING
- **Internal docs** moved to `docs/internal/`:
  - Implementation status, audits, analysis documents
  - Phase summaries, proposals, refactor summaries

### ✅ Cleaned Root Directory
- ❌ Removed: `COMPLETION_REPORT.md`, `SESSION_SUMMARY.md`, `QUICK_START.md`
- ✅ Kept: `README.md`, `CHANGELOG.md`, `LICENSE`, `Cargo.toml`
- ✅ Moved test scripts to `interop_tests/`:
  - `test_interop.sh`, `simple_interop_test.py`, `test_libzmq_client.py`

### ✅ Cleaned Internal Crates
- **monocoque-zmtp**:
  - Removed `benches/` directory (moved to public crate)
  - Removed `examples/` directory (moved to public crate)
  - Removed `[[bench]]` and `[[example]]` from Cargo.toml
  - Kept only library code and internal tests

## Usage Guidelines

### For Library Users
```rust
// ✅ DO: Import from public crate
use monocoque::zmq::DealerSocket;
use monocoque::SocketOptions;

// ❌ DON'T: Import from internal crates
use monocoque_zmtp::RepSocket;  // Won't work - not published!
use monocoque_core::Message;     // Won't work - not published!
```

**Protection**: Internal crates have `publish = false` in their Cargo.toml, so they cannot be added as dependencies from crates.io. Users must use the `monocoque` crate.

### For Contributors
- **Add examples**: `monocoque/examples/`
- **Add benchmarks**: `monocoque/benches/`
- **Add docs**: `docs/` (user-facing) or `docs/internal/` (implementation details)
- **Add scripts**: `scripts/`
- **Modify internals**: `monocoque-core/` or `monocoque-zmtp/`

## Build Commands

```bash
# Build workspace
cargo build --workspace

# Build public crate only
cargo build -p monocoque --features zmq

# Run benchmarks
./scripts/bench_all.sh

# Run interop tests
./scripts/run_interop_tests.sh

# Run fuzzer (10 seconds)
./scripts/run_fuzzer.sh

# Run fuzzer (custom time in seconds)
./scripts/run_fuzzer.sh 60

# Or run fuzzer directly
cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_decoder -- -max_total_time=10
```

## Benefits of This Structure

1. **Clear API boundary**: Users only see `monocoque` crate
2. **Internal flexibility**: Can refactor `-core` and `-zmtp` without breaking users
3. **Organized examples**: All in one place (`monocoque/examples/`)
4. **Consolidated benchmarks**: Easy to find and run
5. **Clean documentation**: User docs vs internal implementation docs
6. **No duplicate scripts**: Single source of truth in `scripts/`

## Related Documentation
- [README.md](README.md) - Project overview with structure diagram
- [docs/GETTING_STARTED.md](docs/GETTING_STARTED.md) - Quick start guide
- [docs/PUBLISHING.md](docs/PUBLISHING.md) - Publishing guidelines
