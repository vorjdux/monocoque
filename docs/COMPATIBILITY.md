# Protocol Compatibility

## ZMTP Version Support

Monocoque implements **ZMTP 3.x** (ZeroMQ Message Transport Protocol), ensuring compatibility with all modern ZeroMQ versions.

### Supported ZeroMQ Versions

| ZeroMQ Version | ZMTP Version | Status                  | Notes                    |
| -------------- | ------------ | ----------------------- | ------------------------ |
| **4.1.x**      | 3.0          | ✅ **Fully Compatible** | Initial ZMTP 3.0 release |
| **4.2.x**      | 3.1          | ✅ **Fully Compatible** | Added heartbeating       |
| **4.3.x**      | 3.1          | ✅ **Fully Compatible** | Enhanced security        |
| **4.4.x**      | 3.1          | ✅ **Fully Compatible** | Latest stable            |

### Compatibility Implementation

#### Version Negotiation

```rust
// greeting.rs - Accepts any ZMTP 3.x version
let major = src[10];
if major < 3 {
    return Err(ZmtpError::Protocol);
}
// Accepts 3.0, 3.1, 3.2, etc.
```

#### Greeting Format

```rust
// session.rs - Sends ZMTP 3.0 greeting
// Version bytes: [0x03, 0x00]
b.extend_from_slice(&[0x03, 0x00]);
```

**Why 3.0?** Sending ZMTP 3.0 ensures backward compatibility while accepting any 3.x version allows forward compatibility.

### Socket Type Compatibility

| Socket Type | ZMQ 4.1+ | Monocoque | Implementation Status |
| ----------- | -------- | --------- | --------------------- |
| DEALER      | ✅       | ✅        | Phase 2 - Complete    |
| ROUTER      | ✅       | ✅        | Phase 2 - Complete    |
| PUB         | ✅       | ✅        | Phase 3 - Complete    |
| SUB         | ✅       | ✅        | Phase 3 - Complete    |
| REQ         | ✅       | ✅        | Phase 4 - Complete    |
| REP         | ✅       | ✅        | Phase 4 - Complete    |
| PUSH        | ✅       | ✅        | Complete              |
| PULL        | ✅       | ✅        | Complete              |
| XPUB        | ✅       | ✅        | Complete              |
| XSUB        | ✅       | ✅        | Complete              |
| PAIR        | ✅       | ✅        | Complete              |

### Feature Compatibility

#### Core Protocol Features

-   ✅ **NULL Security Mechanism** - No authentication (default)
-   ✅ **ZMTP Framing** - 1-byte and 8-byte length frames
-   ✅ **Multipart Messages** - MORE flag handling
-   ✅ **Command Frames** - READY, PING, PONG commands
-   ✅ **Heartbeating** - ZMTP 3.1 PING/PONG (RFC 23)
-   ✅ **PLAIN Security** - Username/password authentication
-   ✅ **CURVE Security** - CurveZMQ public-key encryption
-   ✅ **ZAP Authentication** - ZeroMQ Authentication Protocol

#### Socket Patterns

-   ✅ **Load Balancing** - ROUTER fair-queuing
-   ✅ **Pub-Sub Filtering** - Topic-based subscriptions
-   ✅ **Asynchronous I/O** - Non-blocking operations
-   ✅ **Request-Reply** - REQ/REP strict state machine with relaxed mode
-   ✅ **Pipeline** - PUSH/PULL patterns with reconnection support
-   ✅ **Automatic Reconnection** - Exponential backoff on all socket types
-   ✅ **Socket Options** - Full `zmq_setsockopt` equivalents via `SocketOptions`

## Interoperability Testing

### Testing Against libzmq

The test suite includes interoperability tests with official libzmq:

```bash
# Install libzmq (ZMQ 4.1+)
sudo apt install libzmq3-dev  # Ubuntu/Debian
brew install zeromq           # macOS

# Run interoperability tests
cargo test --package monocoque --features zmq --test interop_pair
cargo test --package monocoque --features zmq --test interop_router
cargo test --package monocoque --features zmq --test interop_pubsub
cargo test --package monocoque --features zmq --test interop_load_balance
```

### Test Coverage

| Test                   | Description                 | Verifies                    |
| ---------------------- | --------------------------- | --------------------------- |
| `interop_pair`         | DEALER-DEALER communication | Basic framing, handshake    |
| `interop_router`       | ROUTER-DEALER patterns      | Routing IDs, load balancing |
| `interop_pubsub`       | PUB-SUB messaging           | Subscription filtering      |
| `interop_load_balance` | ROUTER fair-queuing         | Worker pool patterns        |

## Migration from libzmq

### API Mapping

```rust
// libzmq (C API)
void *dealer = zmq_socket(ctx, ZMQ_DEALER);
zmq_connect(dealer, "tcp://127.0.0.1:5555");

// Monocoque (Rust async)
use monocoque::zmq::DealerSocket;
let socket = DealerSocket::connect("127.0.0.1:5555").await?;
```

### Protocol Guarantees

1. **Wire Compatibility**: Byte-for-byte compatible with libzmq on the wire
2. **Handshake Compatibility**: Accepts libzmq greetings, sends standard ZMTP 3.0
3. **Frame Compatibility**: Uses identical framing format (flags, length, payload)
4. **Command Compatibility**: READY command format matches RFC 23/ZMTP

### Known Limitations

#### Behavioral Differences

-   **Async-First Design** - No blocking API (by design)
    -   Benefit: Better performance, no thread-per-connection
-   **Type Safety** - Socket types enforced at compile-time
    -   Benefit: Prevents invalid socket combinations (e.g., PUB→PUB)

## Compatibility Testing Checklist

When testing against legacy ZMQ 4.1+ systems:

-   [ ] Verify handshake completes (libzmq accepts ZMTP 3.0)
-   [ ] Test message exchange in both directions
-   [ ] Verify multipart message handling
-   [ ] Test error handling (connection drops, invalid frames)
-   [ ] Validate routing ID assignment (ROUTER sockets)
-   [ ] Check subscription filtering (PUB/SUB)
-   [ ] Test load balancing fairness (ROUTER fair-queue)
-   [ ] Verify interop with different libzmq versions (4.1, 4.2, 4.3)

## Version Detection

To detect peer ZMTP version:

```rust
// In greeting.rs parsing
let major = src[10];  // Byte 10: major version (3)
let minor = src[11];  // Byte 11: minor version (0 or 1)

// 3.0 = ZMQ 4.1
// 3.1 = ZMQ 4.2+
```

Monocoque parses the minor version and supports ZMTP 3.1 features (heartbeating via PING/PONG) when both peers advertise 3.1.

## References

-   [RFC 23/ZMTP](https://rfc.zeromq.org/spec:23/ZMTP/) - ZMTP 3.0 Specification
-   [RFC 37/ZMTP](https://rfc.zeromq.org/spec:37/ZMTP/) - ZMTP 3.1 Extensions
-   [libzmq Releases](https://github.com/zeromq/libzmq/releases) - Official implementation versions
-   [ZeroMQ Guide](https://zguide.zeromq.org/) - Patterns and best practices

## Support

**Minimum Supported Version**: ZeroMQ 4.1 (ZMTP 3.0)

For issues with specific ZMQ versions, please open an issue with:

-   ZeroMQ version (`zmq_version()`)
-   Operating system
-   Wire capture (tcpdump/Wireshark)
-   Error messages from both sides
