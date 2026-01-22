# Identity and Routing Options Implementation

## Overview

Identity and routing options are critical for ROUTER socket behavior and provide
control over how messages are routed in advanced ZeroMQ patterns.

## ZeroMQ Socket Options

### ZMQ_ROUTING_ID (61)
Sets the identity of the socket. For ROUTER sockets, this is the address that
peers will use when sending messages to this socket.

**Type:** Binary data (0-255 bytes)
**Default:** NULL (auto-generated)
**Applies to:** ROUTER, REQ, REP, DEALER

### ZMQ_CONNECT_ROUTING_ID (62)
Sets the identity of the peer to connect to. Used with ROUTER sockets to
assign a specific identity to the next connection.

**Type:** Binary data (1-255 bytes)
**Default:** NULL
**Applies to:** ROUTER

### Related Options

#### ZMQ_ROUTER_MANDATORY (33)
When set, ROUTER socket will return EHOSTUNREACH if message cannot be routed.
**Type:** boolean
**Default:** 0 (false)

#### ZMQ_ROUTER_HANDOVER (56)
If true, ROUTER will hand-over the existing identity to new connections.
**Type:** boolean
**Default:** 0 (false)

## Current State

### Existing Implementation

Check `monocoque-core/src/options.rs`:

```rust
pub struct SocketOptions {
    pub identity: Option<Vec<u8>>,
    // ... other options
}
```

### What Exists
- ✅ `SocketOptions` struct with `identity` field
- ✅ Basic option storage
- ⏭️ ROUTER-specific identity handling
- ⏭️ Identity validation (1-255 bytes, no null prefix for ROUTER)
- ⏭️ Connect-time routing ID assignment
- ⏭️ ROUTER_MANDATORY behavior
- ⏭️ ROUTER_HANDOVER behavior

## Implementation Plan

### Step 1: Enhance SocketOptions (monocoque-core)
**File:** `monocoque-core/src/options.rs`
**Effort:** 1 hour

```rust
pub struct SocketOptions {
    // Existing
    pub identity: Option<Vec<u8>>,
    
    // New
    pub connect_routing_id: Option<Vec<u8>>,
    pub router_mandatory: bool,
    pub router_handover: bool,
    
    // ... existing fields
}

impl SocketOptions {
    /// Set the socket identity (ZMQ_ROUTING_ID).
    ///
    /// # Constraints
    /// - Must be 0-255 bytes
    /// - For ROUTER sockets, cannot start with 0x00
    pub fn with_identity(mut self, identity: Vec<u8>) -> Self {
        self.identity = Some(identity);
        self
    }
    
    /// Set the peer routing ID for next connection (ZMQ_CONNECT_ROUTING_ID).
    ///
    /// # Constraints
    /// - Must be 1-255 bytes
    /// - Cannot be empty or start with 0x00
    pub fn with_connect_routing_id(mut self, routing_id: Vec<u8>) -> Self {
        self.connect_routing_id = Some(routing_id);
        self
    }
    
    /// Enable ROUTER_MANDATORY mode (ZMQ_ROUTER_MANDATORY).
    pub fn with_router_mandatory(mut self, enabled: bool) -> Self {
        self.router_mandatory = enabled;
        self
    }
    
    /// Enable ROUTER_HANDOVER mode (ZMQ_ROUTER_HANDOVER).
    pub fn with_router_handover(mut self, enabled: bool) -> Self {
        self.router_handover = enabled;
        self
    }
}
```

**Tests:**
- Option builder methods
- Default values
- Identity validation

### Step 2: Update ROUTER Identity Handling (monocoque-zmtp)
**File:** `monocoque-zmtp/src/router.rs`
**Effort:** 3-4 hours

#### Current ROUTER Implementation
The ROUTER socket maintains an identity map:
```rust
pub struct RouterSocket<S> {
    base: SocketBase<S>,
    identity_map: HashMap<Bytes, S>,  // identity -> peer stream
    // ...
}
```

#### Enhancements Needed

1. **Identity Assignment on Accept/Connect:**
```rust
impl RouterSocket {
    // When accepting a connection
    async fn accept_connection(&mut self, stream: S) -> io::Result<()> {
        let identity = if let Some(custom_id) = self.base.options.connect_routing_id.take() {
            // Use the assigned routing ID
            Bytes::from(custom_id)
        } else {
            // Auto-generate identity
            self.generate_identity()
        };
        
        // Validate identity
        validate_router_identity(&identity)?;
        
        // Check for handover
        if self.base.options.router_handover {
            if let Some(old_stream) = self.identity_map.remove(&identity) {
                // Close old connection with same identity
                drop(old_stream);
            }
        } else if self.identity_map.contains_key(&identity) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "identity already exists and handover is disabled"
            ));
        }
        
        self.identity_map.insert(identity, stream);
        Ok(())
    }
}
```

2. **ROUTER_MANDATORY Enforcement:**
```rust
impl RouterSocket {
    pub async fn send(&mut self, mut msg: Vec<Bytes>) -> io::Result<()> {
        // First frame is routing identity
        let identity = msg.remove(0);
        
        // Look up peer by identity
        let stream = self.identity_map.get_mut(&identity);
        
        if stream.is_none() && self.base.options.router_mandatory {
            return Err(io::Error::new(
                io::ErrorKind::HostUnreachable,
                format!("no peer with identity {:?}", identity)
            ));
        }
        
        // If not mandatory, silently drop
        let Some(stream) = stream else {
            tracing::warn!(?identity, "dropping message to unknown peer");
            return Ok(());
        };
        
        // Send remaining frames
        // ...
    }
}
```

3. **Identity Validation:**
```rust
fn validate_router_identity(identity: &[u8]) -> io::Result<()> {
    if identity.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity cannot be empty"
        ));
    }
    
    if identity.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity cannot exceed 255 bytes"
        ));
    }
    
    if identity[0] == 0x00 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity cannot start with null byte (reserved for auto-generated IDs)"
        ));
    }
    
    Ok(())
}
```

**Tests:**
- Custom identity assignment
- Auto-generated identity format
- Identity collision handling
- ROUTER_MANDATORY behavior
- ROUTER_HANDOVER behavior
- Identity validation

### Step 3: High-Level API (monocoque)
**File:** `monocoque/src/zmq/router.rs`
**Effort:** 1 hour

```rust
impl RouterSocket {
    /// Set the routing identity for the next connection.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_CONNECT_ROUTING_ID` (62).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::RouterSocket;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut router = RouterSocket::bind("tcp://0.0.0.0:5555").await?.1;
    /// 
    /// // Next connection will be assigned this identity
    /// router.set_connect_routing_id(b"worker-001".to_vec());
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_connect_routing_id(&mut self, id: Vec<u8>) -> io::Result<()> {
        if id.is_empty() || id.len() > 255 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "routing ID must be 1-255 bytes"
            ));
        }
        self.inner.options_mut().connect_routing_id = Some(id);
        Ok(())
    }
    
    /// Enable or disable ROUTER_MANDATORY mode.
    ///
    /// When enabled, sending to an unknown identity returns an error.
    /// When disabled (default), messages to unknown identities are silently dropped.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_ROUTER_MANDATORY` (33).
    pub fn set_router_mandatory(&mut self, enabled: bool) {
        self.inner.options_mut().router_mandatory = enabled;
    }
    
    /// Enable or disable ROUTER_HANDOVER mode.
    ///
    /// When enabled, a new connection with an existing identity will take over
    /// that identity, closing the old connection.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_ROUTER_HANDOVER` (56).
    pub fn set_router_handover(&mut self, enabled: bool) {
        self.inner.options_mut().router_handover = enabled;
    }
}
```

### Step 4: Socket Introspection Updates
**File:** `monocoque/src/zmq/common.rs` (or relevant socket files)
**Effort:** 30 minutes

Update introspection to expose identity-related options:

```rust
pub struct SocketIntrospection {
    // ... existing fields
    pub identity: Option<Vec<u8>>,
    pub router_mandatory: Option<bool>,
    pub router_handover: Option<bool>,
}
```

### Step 5: Documentation and Examples
**Effort:** 1-2 hours

#### Example: Custom Worker Pool with Identities

```rust
use monocoque::zmq::{RouterSocket, DealerSocket, SocketOptions};
use bytes::Bytes;

#[compio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Server with ROUTER_MANDATORY
    let (listener, mut router) = RouterSocket::bind("tcp://0.0.0.0:5555").await?;
    router.set_router_mandatory(true);
    
    // Worker 1 with custom identity
    let mut worker1 = DealerSocket::connect_with_options(
        "tcp://127.0.0.1:5555",
        SocketOptions::default().with_identity(b"worker-001".to_vec())
    ).await?;
    
    // Worker 2 with custom identity
    let mut worker2 = DealerSocket::connect_with_options(
        "tcp://127.0.0.1:5555",
        SocketOptions::default().with_identity(b"worker-002".to_vec())
    ).await?;
    
    // Send work to specific worker
    router.send(vec![
        Bytes::from("worker-001"),
        Bytes::new(), // Delimiter
        Bytes::from("task data")
    ]).await?;
    
    // Worker receives task
    let msg = worker1.recv().await.unwrap();
    println!("Worker 1 got: {:?}", msg);
    
    Ok(())
}
```

#### Example: Connection Handover

```rust
// Enable handover mode
router.set_router_handover(true);

// Old connection with identity "client-A"
let client_old = DealerSocket::connect_with_options(
    "tcp://127.0.0.1:5555",
    SocketOptions::default().with_identity(b"client-A".to_vec())
).await?;

// New connection with same identity - takes over
let client_new = DealerSocket::connect_with_options(
    "tcp://127.0.0.1:5555",
    SocketOptions::default().with_identity(b"client-A".to_vec())
).await?;

// Old connection is now closed
// Router messages to "client-A" go to client_new
```

## Testing Plan

### Unit Tests
1. **Option Validation**
   - Identity length constraints
   - Null byte prefix validation
   - Empty identity handling

2. **ROUTER Behavior**
   - Custom identity assignment
   - Auto-generated identity format
   - Identity collision with/without handover
   - ROUTER_MANDATORY error handling
   - Message routing to known/unknown identities

### Integration Tests
1. **ROUTER-DEALER with Custom Identities**
   - Multiple dealers with custom IDs
   - Router sends to specific dealer
   - Router_mandatory enforcement

2. **Connection Handover**
   - Two clients with same identity
   - Verify old connection closed
   - Verify messages route to new connection

3. **Identity Persistence**
   - Reconnection maintains identity
   - Identity survives across message exchanges

## ZeroMQ Compatibility Matrix

| Option | Supported | Notes |
|--------|-----------|-------|
| ZMQ_ROUTING_ID (61) | ✅ Planned | Set socket identity |
| ZMQ_CONNECT_ROUTING_ID (62) | ✅ Planned | Assign peer identity |
| ZMQ_ROUTER_MANDATORY (33) | ✅ Planned | Error on unknown peer |
| ZMQ_ROUTER_HANDOVER (56) | ✅ Planned | Identity takeover |

## Timeline

- **Day 1 (4 hours)**
  - Enhance SocketOptions
  - Add validation functions
  - Unit tests

- **Day 2 (6 hours)**
  - Update RouterSocket implementation
  - Identity management logic
  - ROUTER_MANDATORY/HANDOVER

- **Day 3 (4 hours)**
  - High-level API
  - Documentation
  - Examples

- **Day 4 (3 hours)**
  - Integration tests
  - Edge case handling
  - Review and polish

**Total: ~17 hours (~2-3 days)**

## Future Enhancements

- **ZMQ_ROUTER_RAW (41)**: Raw mode (no identity frame)
- **ZMQ_PROBE_ROUTER (51)**: Probe router readiness
- **ZMQ_REQ_CORRELATE (52)**: Correlate REQ/REP messages
- **ZMQ_REQ_RELAXED (53)**: Relaxed REQ state machine

## References

- ZeroMQ API: https://zeromq.org/socket-api/
- libzmq options: http://api.zeromq.org/master:zmq-setsockopt
- Monocoque options: `monocoque-core/src/options.rs`
- ROUTER implementation: `monocoque-zmtp/src/router.rs`
