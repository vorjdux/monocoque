# ZAP (ZeroMQ Authentication Protocol) Integration Guide

## Overview

Monocoque supports the ZeroMQ Authentication Protocol (RFC 27) for authenticating client connections using PLAIN or CURVE mechanisms.

ZAP uses an internal request/reply pattern over `inproc://zeromq.zap.01` to separate authentication logic from socket implementation.

## Architecture

```
┌─────────────────┐                  ┌──────────────────┐
│  Server Socket  │                  │   ZAP Handler    │
│  (DEALER/REP)   │                  │   (DEALER/REP)   │
└────────┬────────┘                  └────────┬─────────┘
         │                                    │
         │  1. Receive client connection      │
         │     with credentials               │
         │                                    │
         │  2. Send ZAP Request ──────────────>│
         │     (username, password)           │
         │                                    │
         │  3. Wait for ZAP Response <────────│
         │     (200 OK / 400 Failure)         │
         │                                    │
         │  4. Accept or reject connection    │
         │     based on ZAP response          │
         └────────────────────────────────────┘
```

## Quick Start

### 1. Start ZAP Server

Before accepting authenticated connections, start a ZAP server:

```rust
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::spawn_zap_server;
use std::sync::Arc;

fn setup_zap() -> std::io::Result<()> {
    // Create PLAIN authentication handler
    let mut handler = StaticPlainHandler::new();
    handler.add_user("admin", "secret123");
    handler.add_user("user1", "password1");
    
    // Start ZAP server
    let plain_handler = Arc::new(handler);
    let zap_handler = Arc::new(DefaultZapHandler::new(plain_handler, true));
    spawn_zap_server(zap_handler)?;
    
    Ok(())
}
```

### 2. Server Socket with ZAP

Use the ZAP-enabled handshake for server sockets:

```rust
use monocoque_zmtp::security::plain::plain_server_handshake_zap;

async fn accept_connection(stream: TcpStream) -> io::Result<SocketBase> {
    let socket = plain_server_handshake_zap(
        stream,
        "my-app-domain".to_string(),  // ZAP domain
        Some(Duration::from_secs(5)),  // ZAP timeout
    ).await?;
    
    Ok(socket)
}
```

### 3. Client Socket

Client connects normally with credentials:

```rust
use monocoque_zmtp::security::plain::PlainClientHandshake;

async fn connect_to_server() -> io::Result<TcpSocket> {
    let stream = TcpStream::connect("127.0.0.1:5555").await?;
    
    let handshake = PlainClientHandshake::new("admin", "secret123");
    let socket = TcpSocket::connect_with_handshake(
        stream,
        "my-client",
        handshake,
    ).await?;
    
    Ok(socket)
}
```

## ZAP Request/Response Flow

### Request Format (Multipart)

```text
Frame 0: Version (always "1.0")
Frame 1: Request ID (unique identifier)
Frame 2: Domain (ZAP domain string)
Frame 3: Address (client IP:port)
Frame 4: Identity (client identity)
Frame 5: Mechanism ("PLAIN" or "CURVE")
Frame 6+: Credentials (mechanism-specific)
```

### Response Format (Multipart)

```text
Frame 0: Version ("1.0")
Frame 1: Request ID (matches request)
Frame 2: Status code ("200", "300", "400", "500")
Frame 3: Status text (human-readable)
Frame 4: User ID (authenticated user identifier)
Frame 5: Metadata (optional key-value pairs)
```

## Status Codes

| Code | Meaning | Action |
|------|---------|--------|
| 200 | Success | Accept connection |
| 300 | Temporary error | Retry later |
| 400 | Authentication failure | Reject connection |
| 500 | Internal error | Reject connection |

## PLAIN Mechanism

PLAIN sends username/password in cleartext.

**⚠️ Security Warning**: Only use PLAIN over:
- Localhost connections
- Encrypted transports (TLS, VPN, SSH tunnel)
- Trusted networks

### PLAIN ZAP Request

```text
Frame 6: Username (UTF-8 string)
Frame 7: Password (UTF-8 string)
```

### Example

```rust
// Server-side credential checking
let mut handler = StaticPlainHandler::new();
handler.add_user("alice", "password123");

// Client credentials
let handshake = PlainClientHandshake::new("alice", "password123");
```

## CURVE Mechanism

CURVE provides encryption + authentication using X25519 key exchange.

### CURVE ZAP Request

```text
Frame 6: Client public key (32 bytes)
```

### Example (TODO)

```rust
// Server with CURVE
let server_keypair = CurveKeyPair::generate();
let handshake = CurveServerHandshake::new(server_keypair);

// Client with server's public key
let client_keypair = CurveKeyPair::generate();
let handshake = CurveClientHandshake::new(
    client_keypair,
    server_public_key,
);
```

## Custom ZAP Handlers

Implement `PlainAuthHandler` for custom authentication logic:

```rust
use monocoque_zmtp::security::plain::PlainAuthHandler;

struct LdapAuthHandler {
    ldap_url: String,
}

#[async_trait::async_trait(?Send)]
impl PlainAuthHandler for LdapAuthHandler {
    async fn authenticate(
        &self,
        username: &str,
        password: &str,
        domain: &str,
        address: &str,
    ) -> Result<String, String> {
        // Custom LDAP authentication
        match ldap_authenticate(&self.ldap_url, username, password).await {
            Ok(user_id) => Ok(user_id),
            Err(e) => Err(format!("LDAP auth failed: {}", e)),
        }
    }
}
```

## ZAP Domains

Domains allow different authentication policies for different parts of your application:

```rust
// Web API domain
plain_server_handshake_zap(stream, "web-api".to_string(), timeout).await?;

// Internal services domain
plain_server_handshake_zap(stream, "internal".to_string(), timeout).await?;
```

Your ZAP handler can apply different policies based on domain:

```rust
async fn authenticate(&self, username: &str, ..., domain: &str) -> Result<String, String> {
    match domain {
        "web-api" => self.check_web_credentials(username),
        "internal" => self.check_service_credentials(username),
        _ => Err("Unknown domain".to_string()),
    }
}
```

## Timeout Configuration

ZAP requests timeout if the ZAP handler doesn't respond:

```rust
// 5 second timeout
plain_server_handshake_zap(stream, domain, Some(Duration::from_secs(5))).await?;

// Use default timeout (from socket options)
plain_server_handshake_zap(stream, domain, None).await?;
```

## Testing

Integration tests in `tests/zap_integration.rs`:

```bash
cargo test --test zap_integration
```

Tests cover:
- ✅ Successful authentication
- ✅ Failed authentication (wrong password)
- ✅ ZAP timeout handling
- ✅ PLAIN mechanism
- 🔲 CURVE mechanism (TODO)

## Troubleshooting

### "Connection refused" on ZAP socket

The ZAP server must be started **before** accepting connections:

```rust
spawn_zap_server(handler)?;
compio::time::sleep(Duration::from_millis(100)).await; // Give time to bind
```

### "ZAP request timeout"

1. Check ZAP server is running
2. Check ZAP handler isn't blocking (use async I/O)
3. Increase timeout duration
4. Check inproc transport is working

### Authentication always fails

1. Verify credentials match exactly (case-sensitive)
2. Check ZAP domain is correct
3. Enable debug logging: `RUST_LOG=monocoque_zmtp=debug`
4. Check ZAP handler logic

## Performance Considerations

- ZAP adds latency to connection setup (1 RTT to inproc ZAP handler)
- Typical overhead: 50-500μs depending on handler complexity
- Use connection pooling for high-throughput scenarios
- Cache authentication decisions if possible

## Security Best Practices

1. **Never use PLAIN over untrusted networks**
2. **Use CURVE for production** (encryption + auth)
3. **Implement rate limiting** in custom handlers
4. **Log failed authentication attempts**
5. **Rotate credentials regularly**
6. **Use domains to isolate security policies**

## Next Steps

- [ ] CURVE ZAP integration (Task 1 continuation)
- [ ] ZAP metadata support (RFC 35)
- [ ] Connection caching/pooling
- [ ] Rate limiting in ZAP handler
- [ ] Audit logging

## References

- RFC 27: [ZeroMQ Authentication Protocol](https://rfc.zeromq.org/spec:27/ZAP/)
- RFC 23: [PLAIN Security Mechanism](https://rfc.zeromq.org/spec:23/ZMTP/)
- RFC 26: [CURVE Security Mechanism](https://rfc.zeromq.org/spec:26/CURVEZMQ/)
