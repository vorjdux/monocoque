# Socket API Cleanup Proposal
**Date**: January 24, 2026  
**Status**: DRAFT - Needs Review

---

## Problem Statement

The current socket configuration API has several inconsistencies that create confusion:

### 1. Inconsistent Default Buffer Configurations
Different socket types use different `BufferConfig` defaults without clear rationale:

| Socket Type | Default in `new()` | Default in `from_tcp()` | Rationale |
|-------------|-------------------|------------------------|-----------|
| DEALER | `large()` (16KB) | `large()` (16KB) | ✅ High-throughput async patterns |
| ROUTER | `large()` (16KB) | `large()` (16KB) | ✅ High-throughput routing |
| REQ | `small()` (4KB) | **`large()` (16KB)** ❌ | **MISMATCH** - Low-latency RPC |
| REP | `small()` (4KB) | `small()` (4KB) | ✅ Low-latency RPC |
| PAIR | `default()` (?) | `default()` (?) | ⚠️ Unclear |
| PUSH | `default()` (?) | `default()` (?) | ⚠️ Unclear |
| PULL | `default()` (?) | `default()` (?) | ⚠️ Unclear |
| SUB | `large()` (16KB) | `large()` (16KB) | ✅ High-throughput pub-sub |

**Issue**: REQ socket's `from_tcp()` uses `large()` but `new()` uses `small()` - clear contradiction.

### 2. Confusing `from_tcp_with_config()` Behavior

This method signature is misleading:

```rust
pub async fn from_tcp_with_config(
    stream: TcpStream, 
    config: BufferConfig
) -> io::Result<Self>
```

**What users expect**: "Configure buffer sizes for this TCP stream"

**What it actually does**:
```rust
pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self> {
    let options = SocketOptions::default();  // ❌ Always default options!
    crate::utils::configure_tcp_stream(&stream, &options, "SOCKET")?;
    Self::with_options(stream, config, options).await
}
```

**Problem**: Users cannot customize socket options (timeouts, keepalive, etc.) without using the more verbose `from_tcp_with_options()`. The method name suggests it only affects buffer config, but it also silently sets all socket options to defaults.

### 3. API Bloat - Too Many Methods

Each socket type currently has 6+ creation methods:

```rust
// Generic stream constructors (work with TCP, Unix, etc.)
pub async fn new(stream: S) -> io::Result<Self>;
pub async fn with_config(stream: S, config: BufferConfig) -> io::Result<Self>;
pub async fn with_options(stream: S, config: BufferConfig, options: SocketOptions) -> io::Result<Self>;

// TCP-specific constructors (add TCP_NODELAY + keepalive)
pub async fn from_tcp(stream: TcpStream) -> io::Result<Self>;
pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self>;
pub async fn from_tcp_with_options(stream: TcpStream, config: BufferConfig, options: SocketOptions) -> io::Result<Self>;
```

**Issue**: Users must learn 6 methods just to create a socket. The distinction between `with_config()` and `from_tcp_with_config()` is subtle (TCP optimizations) but critical.

### 4. Unclear BufferConfig::default() Semantics

What is `BufferConfig::default()`? Looking at the code:

```rust
// Used by PAIR, PUSH, PULL
BufferConfig::default()
```

**Question**: Is this `small()` (4KB) or `large()` (16KB)? This should be documented clearly.

---

## Proposed Solutions

### Option A: Consistent Defaults (Minimal Breaking Change)

**Goal**: Fix inconsistencies without changing the API surface.

#### Changes:

1. **Standardize Default Buffer Sizes**
   ```rust
   // High-throughput patterns (async, routing, pub-sub)
   DEALER::new() -> BufferConfig::large()
   ROUTER::new() -> BufferConfig::large()
   SUB::new() -> BufferConfig::large()
   XSUB::new() -> BufferConfig::large()
   
   // Low-latency patterns (synchronous request-reply)
   REQ::new() -> BufferConfig::small()
   REP::new() -> BufferConfig::small()
   
   // Pipeline patterns (balanced)
   PUSH::new() -> BufferConfig::default() = BufferConfig::small()
   PULL::new() -> BufferConfig::default() = BufferConfig::small()
   PAIR::new() -> BufferConfig::default() = BufferConfig::small()
   ```

2. **Fix REQ TCP Mismatch**
   ```rust
   // BEFORE (inconsistent):
   impl ReqSocket<TcpStream> {
       pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
           Self::from_tcp_with_config(stream, BufferConfig::large()).await  // ❌
       }
   }
   
   // AFTER (consistent):
   impl ReqSocket<TcpStream> {
       pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
           Self::from_tcp_with_config(stream, BufferConfig::small()).await  // ✅
       }
   }
   ```

3. **Document BufferConfig::default()**
   ```rust
   impl BufferConfig {
       /// Create default buffer configuration (4KB read, 4KB write).
       /// 
       /// Equivalent to `BufferConfig::small()` - optimized for low-latency
       /// with small messages (< 1KB). Use `BufferConfig::large()` for
       /// high-throughput workloads with larger messages.
       pub const fn default() -> Self {
           Self::small()
       }
   }
   ```

**Pros**: Minimal breaking change, fixes the obvious bugs  
**Cons**: Still have 6 methods per socket type

---

### Option B: Builder Pattern (Moderate Breaking Change)

**Goal**: Reduce API surface with a fluent builder.

```rust
// New API:
let socket = ReqSocket::builder()
    .tcp("127.0.0.1:5555")
    .with_buffer_config(BufferConfig::large())
    .with_options(SocketOptions::default()
        .with_tcp_keepalive(1)
        .with_req_correlate(true))
    .build()
    .await?;

// Or simplified:
let socket = ReqSocket::builder()
    .tcp("127.0.0.1:5555")
    .build()
    .await?;  // Uses smart defaults

// For existing streams:
let socket = ReqSocket::builder()
    .stream(tcp_stream)
    .with_buffer_config(BufferConfig::large())
    .build()
    .await?;
```

**Pros**: 
- Single entry point (`builder()`)
- Self-documenting with method chaining
- Easy to add new options without breaking existing code

**Cons**: 
- Major API change
- More code to write and maintain
- Existing users need to migrate

---

### Option C: Simplified API (Major Breaking Change)

**Goal**: Reduce to 3 core methods per socket.

```rust
// 1. Simple constructor with smart defaults
pub async fn new(stream: S) -> io::Result<Self>;

// 2. Full control (replaces with_options + from_tcp_with_options)
pub async fn with_config(
    stream: S, 
    config: BufferConfig, 
    options: SocketOptions
) -> io::Result<Self>;

// 3. Convenience for TCP streams (handles TCP optimizations automatically)
pub async fn from_tcp_with_config(
    stream: TcpStream, 
    config: BufferConfig, 
    options: SocketOptions  // ✅ No longer silently defaults!
) -> io::Result<Self> {
    crate::utils::configure_tcp_stream(&stream, &options, "SOCKET")?;
    Self::with_config(stream, config, options).await
}
```

**Remove**:
- `with_options()` - merged into `with_config()`
- `from_tcp()` - just use `new(stream)`
- `from_tcp_with_config(stream, config)` - ambiguous, removed

**Pros**: 
- Cleaner API - only 3 methods
- Less confusion - clear separation of concerns
- `with_config()` is the "full control" method

**Cons**: 
- Breaking change for all existing users
- Need migration guide

---

## Recommended Approach

**Phase 1 (Immediate - Non-Breaking)**:
1. ✅ Fix REQ `from_tcp()` to use `small()` instead of `large()`
2. ✅ Document `BufferConfig::default()` clearly
3. ✅ Add deprecation warnings to confusing methods

**Phase 2 (Next Minor Version - Breaking)**:
1. Implement **Option C** (Simplified API)
2. Provide migration guide
3. Keep deprecated methods for one release cycle

---

## Implementation Plan

### Phase 1: Quick Fixes (Today)

```rust
// File: monocoque-zmtp/src/req.rs
impl ReqSocket<TcpStream> {
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        // FIX: Use small() to match new() behavior
        Self::from_tcp_with_config(stream, BufferConfig::small()).await
    }
}

// File: monocoque-core/src/config.rs
impl BufferConfig {
    /// Create default buffer configuration (4KB read, 4KB write).
    /// 
    /// This is equivalent to `BufferConfig::small()` and is optimized
    /// for low-latency with small messages (< 1KB typical).
    /// 
    /// For high-throughput workloads with larger messages (> 10KB),
    /// use `BufferConfig::large()` (16KB buffers).
    pub const fn default() -> Self {
        Self { read_buffer_size: 4096, write_buffer_size: 4096 }
    }
    
    /// Small buffers (4KB) - optimized for low-latency with small messages.
    /// 
    /// Best for: REQ/REP, PAIR, PUSH/PULL with < 1KB messages
    pub const fn small() -> Self {
        Self { read_buffer_size: 4096, write_buffer_size: 4096 }
    }
    
    /// Large buffers (16KB) - optimized for high-throughput with large messages.
    /// 
    /// Best for: DEALER/ROUTER, PUB/SUB with > 10KB messages
    pub const fn large() -> Self {
        Self { read_buffer_size: 16384, write_buffer_size: 16384 }
    }
}
```

### Phase 2: Deprecation Warnings

```rust
// Mark confusing methods as deprecated
#[deprecated(
    since = "0.2.0",
    note = "Use `with_config(stream, config, options)` for full control. \
            This method silently uses SocketOptions::default() which may not \
            be what you want."
)]
pub async fn from_tcp_with_config(stream: TcpStream, config: BufferConfig) -> io::Result<Self> {
    // ...existing implementation...
}
```

### Phase 3: New Simplified API (0.3.0)

See **Option C** above for the new API design.

---

## Discussion Questions

1. **Should we keep 6 methods per socket, or simplify?**
   - Current: 6 methods (confusing but complete)
   - Proposed: 3 methods (cleaner but breaking change)

2. **What should BufferConfig::default() be?**
   - Current: Unclear (appears to be 4KB)
   - Proposed: Explicitly `small()` (4KB) with clear docs

3. **How important is backward compatibility?**
   - If high: Do Phase 1 only
   - If moderate: Do Phase 1 + 2
   - If low: Skip to Phase 3

4. **Should from_tcp_with_config() take options parameter?**
   - Current: NO - creates `SocketOptions::default()` internally
   - Proposed: YES - explicit parameter to avoid surprises

---

## Migration Guide (for Phase 3)

```rust
// OLD API:
let socket = ReqSocket::from_tcp_with_config(
    stream, 
    BufferConfig::large()
).await?;

// NEW API (explicit options):
let socket = ReqSocket::with_config(
    stream,
    BufferConfig::large(),
    SocketOptions::default()
).await?;

// Or use convenience method (handles TCP optimizations):
let socket = ReqSocket::from_tcp_with_config(
    stream,
    BufferConfig::large(),
    SocketOptions::default()  // Now explicit!
).await?;
```

---

## Conclusion

**Immediate Action Required**: Fix REQ socket `from_tcp()` inconsistency.

**Long-term**: Consider simplifying the API to 3 core methods per socket type to reduce confusion and maintenance burden.

**User Impact**: Most users only use `new()` or `from_tcp()`, so the inconsistency in `from_tcp_with_config()` affects a smaller subset of advanced users.
