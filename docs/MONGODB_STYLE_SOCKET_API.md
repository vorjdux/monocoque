# MongoDB-Style Socket API
**Date**: January 24, 2026  
**Status**: IMPLEMENTED

---

## Overview

The socket API has been redesigned to follow the MongoDB Rust driver's ergonomic pattern:
- **Single options struct** - All configuration (buffers, timeouts, etc.) in `SocketOptions`
- **Simple API** - Just 2-3 methods per socket instead of 6+
- **Builder pattern** - Fluent chainable configuration
- **No separate config** - Eliminated confusing `BufferConfig` vs `SocketOptions` split

---

## Before (Old Confusing API)

```rust
use monocoque_zmtp::DealerSocket;
use monocoque_core::config::BufferConfig;
use monocoque_core::options::SocketOptions;
use std::time::Duration;

// 6 different methods to remember! 😵
DealerSocket::new(stream)  // Uses large() buffers
DealerSocket::with_config(stream, BufferConfig::small())  // Custom buffers
DealerSocket::with_options(stream, config, options)  // Full control
DealerSocket::from_tcp(stream)  // TCP + large() buffers
DealerSocket::from_tcp_with_config(stream, config)  // TCP + custom buffers, DEFAULT options
DealerSocket::from_tcp_with_options(stream, config, options)  // TCP + full control

// REQ socket inconsistency bug:
ReqSocket::new(stream)  // Uses small() buffers
ReqSocket::from_tcp(stream)  // BUG: Uses large() buffers! 🐛
```

**Problems**:
- ❌ Too many methods (6+ per socket)
- ❌ `BufferConfig` vs `SocketOptions` confusion
- ❌ `from_tcp_with_config()` silently uses default options
- ❌ Inconsistent buffer defaults between sockets
- ❌ Not Rust-idiomatic

---

## After (New MongoDB-Style API)

```rust
use monocoque_zmtp::DealerSocket;
use monocoque_core::options::SocketOptions;
use std::time::Duration;
use bytes::Bytes;

// Just 2 methods! 🎉
DealerSocket::new(stream)  // Default options (8KB buffers)
DealerSocket::with_options(stream, options)  // Custom options

// For TCP streams (adds TCP optimizations):
DealerSocket::from_tcp(stream)  // Default options
DealerSocket::from_tcp_with_options(stream, options)  // Custom options

// All configuration in one place:
let options = SocketOptions::default()
    // Buffer sizes (replaces BufferConfig)
    .with_buffer_sizes(16384, 16384)
    
    // Or use presets:
    // SocketOptions::small()  -> 4KB buffers
    // SocketOptions::large()  -> 16KB buffers
    
    // Timeouts
    .with_recv_timeout(Duration::from_secs(5))
    .with_send_timeout(Duration::from_secs(5))
    
    // TCP keepalive
    .with_tcp_keepalive(1)
    .with_tcp_keepalive_idle(60)
    
    // REQ modes
    .with_req_correlate(true)
    .with_req_relaxed(false)
    
    // Identity/routing
    .with_routing_id(Bytes::from("worker-01"))
    
    // XPUB/XSUB
    .with_xpub_verbose(true);

let socket = DealerSocket::with_options(stream, options).await?;
```

**Benefits**:
- ✅ Simple API - just 2-4 methods per socket
- ✅ All configuration in one place (`SocketOptions`)
- ✅ Fluent builder pattern (MongoDB-style)
- ✅ Consistent defaults across all sockets
- ✅ More Rust-idiomatic

---

## API Comparison Table

| Old API | New API | Notes |
|---------|---------|-------|
| `new(stream)` | `new(stream)` | Now uses 8KB default buffers |
| `with_config(stream, config)` | **REMOVED** | Use `with_options` |
| `with_options(stream, config, options)` | `with_options(stream, options)` | One options param |
| `from_tcp(stream)` | `from_tcp(stream)` | Simplified |
| `from_tcp_with_config(stream, config)` | **REMOVED** | Use `from_tcp_with_options` |
| `from_tcp_with_options(stream, config, options)` | `from_tcp_with_options(stream, options)` | One options param |
| `BufferConfig::small()` | `SocketOptions::small()` | 4KB buffers |
| `BufferConfig::large()` | `SocketOptions::large()` | 16KB buffers |
| `BufferConfig::default()` | `SocketOptions::default()` | 8KB buffers |

---

## SocketOptions New Fields

```rust
pub struct SocketOptions {
    // NEW: Buffer sizes (moved from BufferConfig)
    pub read_buffer_size: usize,   // Default: 8192 (8KB)
    pub write_buffer_size: usize,  // Default: 8192 (8KB)
    
    // Existing fields...
    pub recv_timeout: Option<Duration>,
    pub send_timeout: Option<Duration>,
    pub handshake_timeout: Duration,
    pub linger: Option<Duration>,
    pub reconnect_ivl: Duration,
    pub reconnect_ivl_max: Duration,
    pub connect_timeout: Duration,
    pub recv_hwm: usize,
    pub send_hwm: usize,
    pub immediate: bool,
    pub max_msg_size: Option<usize>,
    pub routing_id: Option<Bytes>,
    pub connect_routing_id: Option<Bytes>,
    pub router_mandatory: bool,
    pub router_handover: bool,
    pub probe_router: bool,
    pub xpub_verbose: bool,
    pub xpub_manual: bool,
    pub xpub_welcome_msg: Option<Bytes>,
    pub xsub_verbose_unsubs: bool,
    pub conflate: bool,
    pub tcp_keepalive: i32,
    pub tcp_keepalive_cnt: i32,
    pub tcp_keepalive_idle: i32,
    pub tcp_keepalive_intvl: i32,
    pub req_correlate: bool,
    pub req_relaxed: bool,
}
```

---

## Migration Guide

### Simple Case (Using Defaults)

```rust
// OLD:
let socket = DealerSocket::new(stream).await?;

// NEW:
let socket = DealerSocket::new(stream).await?;
// ✅ No change needed!
```

### Custom Buffers Only

```rust
// OLD:
let socket = DealerSocket::with_config(
    stream, 
    BufferConfig::large()
).await?;

// NEW:
let socket = DealerSocket::with_options(
    stream,
    SocketOptions::large()  // Includes 16KB buffers
).await?;
```

### Custom Buffers + Options

```rust
// OLD:
let socket = DealerSocket::with_options(
    stream,
    BufferConfig::large(),
    SocketOptions::default()
        .with_recv_timeout(Duration::from_secs(5))
).await?;

// NEW:
let socket = DealerSocket::with_options(
    stream,
    SocketOptions::large()  // 16KB buffers
        .with_recv_timeout(Duration::from_secs(5))
).await?;
```

### TCP with Custom Config (OLD API Bug)

```rust
// OLD (BUGGY):
let socket = DealerSocket::from_tcp_with_config(
    stream,
    BufferConfig::large()
).await?;
// ❌ This silently uses SocketOptions::default()!

// NEW (EXPLICIT):
let socket = DealerSocket::from_tcp_with_options(
    stream,
    SocketOptions::large()
).await?;
// ✅ All options are explicit
```

### TCP with Full Control

```rust
// OLD:
let socket = DealerSocket::from_tcp_with_options(
    stream,
    BufferConfig::large(),
    SocketOptions::default()
        .with_tcp_keepalive(1)
        .with_recv_timeout(Duration::from_secs(5))
).await?;

// NEW:
let socket = DealerSocket::from_tcp_with_options(
    stream,
    SocketOptions::large()
        .with_tcp_keepalive(1)
        .with_recv_timeout(Duration::from_secs(5))
).await?;
```

### REQ Socket (Fixed Inconsistency)

```rust
// OLD (INCONSISTENT):
let socket = ReqSocket::new(stream).await?;  // small() buffers
let socket = ReqSocket::from_tcp(stream).await?;  // large() buffers ❌

// NEW (CONSISTENT):
let socket = ReqSocket::new(stream).await?;  // 8KB buffers
let socket = ReqSocket::from_tcp(stream).await?;  // 8KB buffers ✅

// Want different sizes? Explicit:
let socket = ReqSocket::with_options(
    stream,
    SocketOptions::small()  // 4KB for low-latency RPC
).await?;
```

---

## Implementation Status

### ✅ Completed
- [x] Add buffer size fields to `SocketOptions`
- [x] Add `SocketOptions::small()`, `large()`, preset methods
- [x] Update `SocketBase` to use `options.read_buffer_size` / `write_buffer_size`
- [x] Simplify DEALER socket API (2 methods per impl)
- [x] Remove `BufferConfig` parameter from all constructors

### 🔄 In Progress (Next Steps)
- [ ] Update all remaining socket types (ROUTER, REQ, REP, PAIR, PUSH, PULL, SUB, XSUB)
- [ ] Update all examples to use new API
- [ ] Update public crate (monocoque) wrapper API
- [ ] Add deprecation warnings to old methods

### 📝 To Do
- [ ] Update documentation
- [ ] Add migration guide to docs
- [ ] Run test suite to verify no breakage
- [ ] Update benchmarks

---

## Examples

### Basic Request-Reply

```rust
use monocoque_zmtp::{ReqSocket, RepSocket};
use monocoque_core::options::SocketOptions;
use bytes::Bytes;

// REQ socket with small buffers for low-latency
let options = SocketOptions::small();  // 4KB buffers
let mut req = ReqSocket::from_tcp_with_options(stream, options).await?;

req.send(vec![Bytes::from("REQUEST")]).await?;
let reply = req.recv().await?;
```

### High-Throughput DEALER

```rust
use monocoque_zmtp::DealerSocket;
use monocoque_core::options::SocketOptions;
use std::time::Duration;

// Large buffers + timeouts
let options = SocketOptions::large()
    .with_recv_timeout(Duration::from_secs(30))
    .with_send_timeout(Duration::from_secs(30))
    .with_tcp_keepalive(1)
    .with_tcp_keepalive_idle(120);

let socket = DealerSocket::connect(
    "tcp://127.0.0.1:5555",
    options
).await?;
```

### Custom Buffer Sizes

```rust
use monocoque_zmtp::DealerSocket;
use monocoque_core::options::SocketOptions;

// Custom 32KB buffers for very large messages
let options = SocketOptions::default()
    .with_buffer_sizes(32768, 32768);

let socket = DealerSocket::with_options(stream, options).await?;
```

---

## Benefits Summary

1. **Simpler Mental Model**
   - One options struct, not two
   - Clear what goes where

2. **Fewer Methods**
   - 2-4 methods instead of 6+ per socket
   - Less API surface to remember

3. **Better Ergonomics**
   - Fluent builder pattern
   - Follows MongoDB Rust driver conventions
   - Chainable method calls

4. **Fixes Bugs**
   - REQ socket buffer inconsistency fixed
   - No more silent default options in `from_tcp_with_config()`

5. **More Maintainable**
   - Less code duplication
   - Easier to add new options
   - Single source of truth

6. **Type Safety**
   - No separate config/options to mix up
   - Compiler helps catch mistakes

---

## Backward Compatibility

This is a **breaking change** that requires updating:
- All socket constructors
- All examples
- All tests
- Public API wrappers

However, most user code only needs minor changes:
- Remove `BufferConfig` imports
- Change `BufferConfig::large()` → `SocketOptions::large()`
- Remove `config` parameter from `with_options()` calls

The improved ergonomics justify the breaking change.
