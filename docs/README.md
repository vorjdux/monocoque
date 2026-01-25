# Monocoque Documentation Index

**Last Updated:** January 25, 2026  
**Status:** Production Ready

---

## Quick Start

New to Monocoque? Start here:

1. [Getting Started Guide](GETTING_STARTED.md)
2. [Integration Guide](INTEGRATION_GUIDE.md)
3. [Examples](../examples/)

---

## Core Documentation

### Protocol Implementation

- **[Implementation Status](IMPLEMENTATION_STATUS.md)** - Feature completeness checklist
- **[Compatibility](COMPATIBILITY.md)** - ZeroMQ compatibility matrix
- **[ZMTP Protocol](ZMTP_PROTOCOL.md)** - Wire protocol details
- **[Socket Patterns](SOCKET_PATTERNS.md)** - REQ/REP, PUB/SUB, etc.

### Security

- **[ZAP Integration Guide](ZAP_INTEGRATION_GUIDE.md)** ⭐ NEW
  - Authentication protocol (RFC 27)
  - PLAIN and CURVE mechanisms
  - Custom handlers
  - Production setup

- **[Security Audit](SECURITY_AUDIT.md)** ⭐ NEW
  - Threat model
  - Cryptographic implementation review
  - Attack surface analysis
  - Security checklist

### Performance

- **[Performance Benchmarks](PERFORMANCE.md)** 
  - 31-35% faster latency than libzmq
  - 3.24M msg/s throughput
  - Detailed benchmark results

### Production Operations

- **[Production Deployment Guide](PRODUCTION_DEPLOYMENT.md)** ⭐ NEW
  - Pre-deployment checklist
  - Security configuration (CURVE, ZAP)
  - Performance tuning
  - Monitoring & observability
  - Error handling patterns
  - Migration from libzmq
  - Kubernetes/Docker deployment
  - Troubleshooting

- **[Fuzzing Guide](FUZZING.md)** ⭐ NEW
  - Setting up fuzzing
  - Fuzz targets
  - Crash triage
  - Continuous fuzzing

---

## Implementation Guides

### Features

- **[Identity & Routing](IDENTITY_ROUTING_OPTIONS.md)**
- **[TCP Keepalive](IMPLEMENTATION_TCP_KEEPALIVE_REQ_MODES.md)**
- **[InProc Transport](INPROC_IMPLEMENTATION.md)**
- **[MongoDB-Style API](MONGODB_STYLE_SOCKET_API.md)**

### Development

- **[Next Steps Analysis](NEXT_STEPS_ANALYSIS.md)**
- **[Implementation Next Steps](IMPLEMENTATION_NEXT_STEPS.md)**
- **[Interop Testing](INTEROP_TESTING.md)**

---

## Testing

### Integration Tests

Located in `../tests/`:

- **ZAP Integration** (`zap_integration.rs`)
  - PLAIN authentication success/failure
  - Timeout handling
  - CURVE authentication (TODO)

- **REQ State Machine** (`req_state_machine.rs`)
  - Strict mode enforcement
  - Relaxed mode
  - Correlation mode
  - 6 comprehensive tests

### Interop Tests

Located in `../interop_tests/`:

- **REQ/REP Interop** (`test_req_rep_interop.py`)
  - libzmq ↔ Monocoque compatibility
  - Multipart messages
  - Large messages (1MB)

- **PUB/SUB Interop** (`test_pub_sub_interop.py`)
  - Publisher/Subscriber patterns
  - Topic filtering

**Running:**
```bash
cd interop_tests
pytest -v
```

---

## Benchmarks

Located in `../benchmarks/`:

- **libzmq Throughput** (`libzmq_throughput.py`)
  - REQ/REP latency comparison
  - PUB/SUB throughput
  - Multiple message sizes

**Running:**
```bash
python benchmarks/libzmq_throughput.py
cargo bench
```

---

## Fuzzing

Located in `../fuzz/`:

- **ZMTP Decoder** (`fuzz_targets/fuzz_decoder.rs`)
  - Malformed frame handling
  - Invalid lengths
  - State machine corruption

**Running:**
```bash
cd fuzz
cargo fuzz run fuzz_decoder
```

---

## Examples

Located in `../examples/`:

### Basic Patterns
- `req_client.rs` - REQ socket client
- `rep_server.rs` - REP socket server
- `pub_server.rs` - PUB socket publisher
- `sub_client.rs` - SUB socket subscriber

### Advanced
- `dealer_client.rs` - DEALER socket
- `router_server.rs` - ROUTER socket
- `stream_adapter.rs` - Stream/Sink usage
- `xpub_xsub.rs` - XPUB/XSUB proxy

---

## Session Reports

- **[Session Summary](../SESSION_SUMMARY.md)** - Tasks 1-5 completion
- **[Completion Report](../COMPLETION_REPORT.md)** ⭐ NEW - Tasks 6-10 completion
  - All 10 tasks complete (100%)
  - Production readiness assessment
  - Files created summary
  - Next steps

---

## Configuration

### Socket Options

See `monocoque-core/src/options.rs`:

- `recv_buffer_size` / `send_buffer_size` - Buffer sizes
- `tcp_nodelay` - Disable Nagle's algorithm
- `tcp_keepalive` - TCP keepalive settings
- `req_relaxed` - REQ relaxed mode
- `req_correlate` - Request ID correlation
- `handshake_timeout` - ZMTP handshake timeout
- `connect_timeout` - Connection timeout

### Default Configurations

```rust
// Low latency
SocketOptions::small()  // 4KB buffers, TCP_NODELAY

// Balanced
SocketOptions::default()  // 8KB buffers

// High throughput  
SocketOptions::large()  // 16KB buffers
```

---

## API Documentation

Generate with:
```bash
cargo doc --open
```

Key modules:
- `monocoque::req` - REQ socket
- `monocoque::rep` - REP socket
- `monocoque::pub` - PUB socket
- `monocoque::sub` - SUB socket
- `monocoque::dealer` - DEALER socket
- `monocoque::router` - ROUTER socket
- `monocoque::security` - PLAIN, CURVE, ZAP

---

## Quick Reference

### REQ/REP Example

```rust
// Server (REP)
let listener = TcpListener::bind("127.0.0.1:5555").await?;
let (stream, _) = listener.accept().await?;
let mut socket = RepSocket::new(stream).await?;

loop {
    if let Some(msg) = socket.recv().await? {
        socket.send(vec![Bytes::from("Reply")]).await?;
    }
}

// Client (REQ)
let stream = TcpStream::connect("127.0.0.1:5555").await?;
let mut socket = ReqSocket::new(stream).await?;

socket.send(vec![Bytes::from("Request")]).await?;
let reply = socket.recv().await?;
```

### CURVE Encryption

```rust
// Server
let keypair = CurveKeyPair::generate();
let handshake = CurveServerHandshake::new(keypair);
let socket = TcpSocket::connect_with_handshake(stream, "server", handshake).await?;

// Client
let server_public = CurvePublicKey::from_hex(&public_key_hex)?;
let client_keypair = CurveKeyPair::generate();
let handshake = CurveClientHandshake::new(client_keypair, server_public);
let socket = TcpSocket::connect_with_handshake(stream, "client", handshake).await?;
```

### ZAP Authentication

```rust
// Start ZAP server
let mut handler = StaticPlainHandler::new();
handler.add_user("admin", "secret123");
spawn_zap_server(Arc::new(DefaultZapHandler::new(Arc::new(handler), true)))?;

// Use ZAP-enabled handshake
let socket = plain_server_handshake_zap(stream, "domain".to_string(), timeout).await?;
```

---

## Getting Help

- **Issues:** https://github.com/monocoque/issues
- **Discussions:** https://github.com/monocoque/discussions
- **Security:** security@monocoque.rs

---

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md) for:
- Code style guidelines
- Testing requirements
- Pull request process
- Development setup

---

## License

See [LICENSE](../LICENSE)

---

## Changelog

See [CHANGELOG.md](../CHANGELOG.md) for version history

---

## Documentation Organization

```
docs/
├── README.md (this file)
├── GETTING_STARTED.md
├── INTEGRATION_GUIDE.md
├── ZAP_INTEGRATION_GUIDE.md        ⭐ NEW
├── SECURITY_AUDIT.md                ⭐ NEW
├── PRODUCTION_DEPLOYMENT.md         ⭐ NEW
├── FUZZING.md                       ⭐ NEW
├── PERFORMANCE.md
├── IMPLEMENTATION_STATUS.md
├── COMPATIBILITY.md
└── [other guides...]

tests/
├── zap_integration.rs               ⭐ NEW
└── req_state_machine.rs             ⭐ NEW

interop_tests/                        ⭐ NEW
├── README.md
├── test_req_rep_interop.py
└── test_pub_sub_interop.py

benchmarks/
└── libzmq_throughput.py             ⭐ NEW

fuzz/                                 ⭐ NEW
├── Cargo.toml
└── fuzz_targets/
    └── fuzz_decoder.rs

examples/
├── rep_server.rs                    ⭐ NEW
├── req_client.rs                    ⭐ NEW
└── [other examples...]
```

---

**Total Documentation:** 15+ guides, 2,000+ lines of docs

**Status:** ✅ Production Ready
