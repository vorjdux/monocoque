# Inproc Transport Implementation

## Status: Core Complete, Socket Integration Pending

### Completed Work

#### 1. Core Transport Layer (`monocoque-core/src/inproc.rs`)
✅ **Complete and Tested**

- Zero-copy in-process transport using flume channels
- Global registry with DashMap for thread-safe endpoint management
- Runtime-agnostic design (works with compio, tokio, or std::thread)
- Full API:
  - `bind_inproc(endpoint)` - Bind to inproc endpoint, returns (sender, receiver)
  - `connect_inproc(endpoint)` - Connect to bound endpoint, returns sender
  - `unbind_inproc(endpoint)` - Remove endpoint from registry
  - `list_inproc_endpoints()` - List all active endpoints
  - `validate_and_extract_name(endpoint)` - Parse and validate URIs

**Tests:** 4/4 passing
- Endpoint validation
- Duplicate bind detection
- Bind and connect flow
- Endpoint listing

**Dependencies Added:**
- `dashmap = "6.0"` - Concurrent hashmap for registry
- `once_cell = "1.19"` - Lazy static initialization

#### 2. Endpoint Support (`monocoque-core/src/endpoint.rs`)
✅ **Complete and Tested**

- Added `Endpoint::Inproc(String)` variant
- Updated parsing to handle `inproc://name` URIs
- Empty name validation
- Display formatting for inproc endpoints
- Helper method `is_inproc()`

**Tests:** 9/9 passing (including 2 new inproc tests)

#### 3. Demo Application (`monocoque/examples/inproc_demo.rs`)
✅ **Complete and Working**

Demonstrates:
- Binding and connecting to inproc endpoints
- Multi-client communication
- Multi-part messages
- Zero-copy 100KB payload transfer
- Endpoint listing
- Cleanup

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    monocoque/src/zmq                        │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                 │
│  │ DEALER   │  │ ROUTER   │  │   REQ    │  (High-level    │
│  │ Socket   │  │ Socket   │  │  Socket  │   API)          │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘                 │
│       │             │              │                        │
│       └─────────────┴──────────────┘                       │
│                     │                                       │
└─────────────────────┼───────────────────────────────────────┘
                      │
┌─────────────────────┼───────────────────────────────────────┐
│                     ▼         monocoque-zmtp                │
│  ┌────────────────────────────────────────┐                │
│  │  Internal Socket Implementations       │                │
│  │  (ZMTP protocol, connection management) │                │
│  └────────────────┬───────────────────────┘                │
│                   │                                          │
└───────────────────┼──────────────────────────────────────────┘
                    │
┌───────────────────┼──────────────────────────────────────────┐
│                   ▼         monocoque-core                   │
│  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐       │
│  │   Endpoint   │  │   Inproc     │  │  Message    │       │
│  │   Parsing    │  │  Transport   │  │   Types     │       │
│  │              │  │              │  │             │       │
│  │ • Tcp        │  │ • Registry   │  │ • Frames    │       │
│  │ • Ipc        │  │ • Channels   │  │ • Routing   │       │
│  │ • Inproc ✅  │  │ • bind/conn  │  │             │       │
│  └──────────────┘  └──────────────┘  └─────────────┘       │
│                                                              │
│  [✅ COMPLETE - Core infrastructure ready]                  │
└──────────────────────────────────────────────────────────────┘
```

### Design Decisions

#### 1. Why flume (not tokio)?
❌ **Initial mistake:** Used tokio::sync::mpsc
✅ **Corrected:** Using flume

**Reasons:**
- monocoque uses compio (io_uring), not tokio
- Mixing async runtimes causes conflicts and undefined behavior
- flume is runtime-agnostic (works with any or no runtime)
- Already a workspace dependency
- Provides efficient MPMC unbounded channels

#### 2. Synchronous connect_inproc
- No need for async in in-process communication
- Immediate connection (no I/O, no network)
- Simpler API, less overhead

#### 3. Global Registry Pattern
```rust
static INPROC_REGISTRY: Lazy<DashMap<String, InprocSender>> = ...
```
- DashMap provides lock-free concurrent access
- Matches ZeroMQ's global inproc namespace
- Thread-safe without explicit locking

### Remaining Work

#### Phase 1: ZMTP Layer Integration ⚠️ **BLOCKED**
**Priority: HIGH**
**Effort: 5-7 days** (increased from 3-4 days)
**Status: In Progress - Architectural Challenge Discovered**

**Challenge Discovered:** The current inproc design has fundamental limitations:

1. **Unidirectional Channels**: The `bind_inproc()` creates unidirectional channels
   - Bind returns (sender, receiver)
   - Connect returns only sender
   - This works for unidirectional patterns (PUB→SUB, PUSH→PULL)
   - **Doesn't work for bidirectional patterns** (PAIR, DEALER↔ROUTER, REQ↔REP)

2. **compio AsyncRead/AsyncWrite Incompatibility**: 
   - Attempted to create `InprocStream` adapter
   - compio's traits are different from tokio's (buffer-based vs poll-based)
   - compio AsyncRead/AsyncWrite require different method signatures:
     ```rust
     async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B>
     async fn write<T: IoBuf>(&mut self, buf: T) -> BufResult<usize, T>
     ```
   - Cannot directly adapt flume channels to compio's async traits

**Attempted Solutions:**
- ✅ Created `InprocStream` wrapper around flume channels
- ❌ Failed: compio traits require async methods, flume is sync
- ❌ Failed: Unidirectional channels don't support PAIR semantics

**Possible Solutions:**

##### Solution A: Bidirectional Inproc Architecture (Recommended)
Redesign inproc to support bidirectional communication:

```rust
// New inproc API
pub struct InprocConnection {
    tx: Sender<InprocMessage>,
    rx: Receiver<InprocMessage>,
}

pub fn bind_inproc(endpoint: &str) -> io::Result<InprocConnection>;
pub fn connect_inproc(endpoint: &str) -> io::Result<InprocConnection>;

// Registry stores bidirectional connection points
static INPROC_REGISTRY: Lazy<DashMap<String, (Sender<...>, Receiver<...>)>>
```

**Benefits:**
- Supports all socket patterns
- Clean bidirectional semantics
- Still zero-copy with Arc

**Drawbacks:**
- Breaking change to inproc API
- More complex registry management
- Need channel pairing protocol

**Effort:** 3-4 days

##### Solution B: Bypass ZMTP for Inproc
Create dedicated inproc socket implementations that don't use ZMTP layer:

```rust
// Direct channel-based socket (no ZMTP, no AsyncRead/AsyncWrite)
pub struct InprocPairSocket {
    tx: InprocSender,
    rx: InprocReceiver,
}

impl InprocPairSocket {
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()>;
    pub async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>>;
}
```

**Benefits:**
- Simpler implementation
- No compio trait compatibility issues
- Can be implemented quickly
- Still zero-copy

**Drawbacks:**
- Separate code path for inproc vs TCP/IPC
- Code duplication for each socket type
- Less elegant architecture

**Effort:** 2-3 days per socket type

##### Solution C: compio-Compatible InprocStream (Hard)
Properly implement compio's AsyncRead/AsyncWrite:

```rust
impl AsyncRead for InprocStream {
    async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        // Block on channel recv, then copy to buffer
        match self.rx.recv() {
            Ok(msg) => {
                let data = msg.concat(); // Flatten frames
                let n = buf.as_mut_slice().len().min(data.len());
                buf.as_mut_slice()[..n].copy_from_slice(&data[..n]);
                (Ok(n), buf)
            }
            Err(_) => (Ok(0), buf), // EOF
        }
    }
}
```

**Benefits:**
- Unified architecture (all sockets use SocketBase<S>)
- Inproc works like any other transport
- Clean abstraction

**Drawbacks:**
- Complex buffer management
- May lose some zero-copy benefits
- Still need bidirectional channel solution
- compio's buffer ownership semantics are tricky

**Effort:** 4-5 days

**Recommendation:** Implement **Solution B** first for PAIR sockets as a proof of concept,
then evaluate if **Solution A** is worth the effort for full bidirectional support.

#### Phase 1.5: Current Work Status
**Files Created:**
- `monocoque-zmtp/src/inproc_stream.rs` - Attempted InprocStream adapter (doesn't compile)
- `monocoque/examples/inproc_pair_demo.rs` - Demo showing bidirectional challenge

**Files Modified:**
- `monocoque-zmtp/src/pair.rs` - Added bind_inproc/connect_inproc stubs (incomplete)
- `monocoque-zmtp/src/lib.rs` - Added inproc_stream module

**Compilation Errors:**
- InprocStream doesn't implement compio::AsyncRead/AsyncWrite correctly
- Missing match arms for `Endpoint::Inproc` in dealer.rs and base.rs
- Unused variables in incomplete pair.rs implementation

**Next Steps:**
1. Decide on architecture (Solution A, B, or C)
2. Implement chosen solution
3. Add tests
4. Update documentation

#### Phase 2: Socket API Integration
**Priority: HIGH**
**Effort: 2-3 days**

Update high-level socket APIs in `monocoque/src/zmq/`:

1. Add inproc methods to all socket types:
   ```rust
   impl DealerSocket {
       pub fn connect_inproc(endpoint: &str) -> io::Result<Self>
       pub fn bind_inproc(endpoint: &str) -> io::Result<Self>
   }
   ```

2. Update existing `connect()` to auto-detect inproc:
   ```rust
   pub async fn connect(endpoint: &str) -> io::Result<Self> {
       match Endpoint::parse(endpoint)? {
           Endpoint::Tcp(addr) => Self::connect_tcp(addr).await,
           Endpoint::Inproc(name) => Self::connect_inproc(&name),
           // ...
       }
   }
   ```

3. Update documentation and examples

#### Phase 3: Testing & Validation
**Priority: MEDIUM**
**Effort: 2-3 days**

1. Unit tests for inproc sockets
2. Integration tests:
   - DEALER ↔ ROUTER over inproc
   - PUB ↔ SUB over inproc
   - REQ ↔ REP over inproc
3. Benchmark inproc vs TCP/IPC
4. Test mixed transports (TCP + inproc simultaneously)

#### Phase 4: Advanced Features (Optional)
**Priority: LOW**
**Effort: 1-2 days**

- High water marks for inproc channels
- Statistics/introspection for inproc endpoints
- Bounded channel support (memory limits)

### Testing Strategy

#### Current Test Coverage
```
monocoque-core:
  ✅ inproc module: 4/4 tests passing
  ✅ endpoint module: 9/9 tests passing (includes inproc)
  ✅ inproc_demo example: working correctly
```

#### Needed Tests (Post-Integration)
```
monocoque-zmtp:
  - Inproc DEALER send/recv
  - Inproc ROUTER routing
  - Error handling (endpoint not bound, etc.)
  - Multi-connection scenarios

monocoque:
  - Socket API consistency
  - Event monitoring for inproc
  - Reconnection behavior
  - Mixed transport scenarios
```

### Performance Expectations

Based on the zero-copy design:

**Expected throughput:**
- **Inproc:** ~10-50M msg/sec (flume + Arc)
- **TCP localhost:** ~1-2M msg/sec
- **IPC (Unix sockets):** ~3-5M msg/sec

**Expected latency:**
- **Inproc:** <1μs (single channel send)
- **TCP localhost:** ~20-50μs
- **IPC:** ~5-15μs

### API Usage Example (Future)

```rust
use monocoque::zmq::{DealerSocket, RouterSocket};
use bytes::Bytes;

// Server
let (_, mut router) = RouterSocket::bind_inproc("inproc://server")?;

// Client
let mut dealer = DealerSocket::connect_inproc("inproc://server")?;

// Zero-copy communication in same process
dealer.send(vec![Bytes::from("fast message")]).await?;
let msg = router.recv().await.unwrap();
```

### Compatibility Notes

**ZeroMQ compatibility:**
- ✅ URI scheme: `inproc://name`
- ✅ Global namespace
- ✅ Bind-before-connect enforcement
- ⚠️ No ZMTP handshake (optimization)
- ⚠️ Async API (monocoque uses compio)

**Differences from libzmq:**
- Uses flume channels instead of custom queues
- No HWM support yet (unbounded)
- Simpler binding model (single bind per endpoint)

### Migration Path

For users wanting to switch from TCP to inproc:

```rust
// Before (TCP)
let socket = DealerSocket::connect("tcp://127.0.0.1:5555").await?;

// After (inproc)
let socket = DealerSocket::connect_inproc("inproc://my-service")?;
```

**Benefits:**
- 10-50x faster
- Zero TCP overhead
- No port conflicts
- Simpler deployment

**Trade-offs:**
- Same-process only
- No network isolation
- Different error modes

### References

- **Core implementation:** `monocoque-core/src/inproc.rs`
- **Endpoint support:** `monocoque-core/src/endpoint.rs`
- **Demo:** `monocoque/examples/inproc_demo.rs`
- **Commits:**
  - `c51a10b` - Inproc transport core
  - `ac4c951` - Endpoint inproc support

### Next Steps

1. ✅ Complete core transport (DONE)
2. ✅ Complete endpoint support (DONE)
3. ⏭️ Integrate with monocoque-zmtp layer (NEXT)
4. ⏭️ Update socket APIs
5. ⏭️ Add comprehensive tests
6. ⏭️ Performance benchmarks
7. ⏭️ Update documentation

**Estimated time to full integration:** 7-10 days of development
