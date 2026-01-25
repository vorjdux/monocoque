# Security Guide for Monocoque ZeroMQ

**Last Updated**: January 25, 2026  
**Status**: Production Ready  
**Coverage**: PLAIN Authentication, CURVE Encryption, ZAP Protocol

---

## Table of Contents

1. [Overview](#overview)
2. [NULL Security (Development Only)](#null-security)
3. [PLAIN Authentication](#plain-authentication)
4. [CURVE Encryption](#curve-encryption)
5. [ZAP Protocol](#zap-protocol)
6. [Best Practices](#best-practices)
7. [Migration from libzmq](#migration-from-libzmq)
8. [Security Checklist](#security-checklist)
9. [Troubleshooting](#troubleshooting)

---

## Overview

Monocoque implements three security mechanisms from the ZeroMQ specification:

| Mechanism | RFC | Use Case | Encryption | Authentication |
|-----------|-----|----------|------------|----------------|
| **NULL** | RFC 23 | Development/trusted networks | ❌ No | ❌ No |
| **PLAIN** | RFC 23 | Simple username/password | ❌ No | ✅ Yes |
| **CURVE** | RFC 26 | Production security | ✅ Yes | ✅ Yes |

### Security Model

ZeroMQ security is **connection-based**, not message-based:
- Authentication happens during the ZMTP handshake
- Security mechanism is negotiated before any application messages
- Once authenticated, all messages on that connection are trusted
- CURVE provides message-level encryption and authentication

---

## NULL Security

**⚠️ WARNING**: NULL mechanism provides **no security**. Use only in development or on trusted networks.

### Configuration

```rust
use monocoque_core::SocketOptions;

// Default is NULL - no configuration needed
let options = SocketOptions::new();
```

### When to Use NULL
- ✅ Development and testing
- ✅ Localhost-only communication (inproc, tcp://127.0.0.1)
- ✅ Private networks behind firewalls
- ❌ **NEVER** use on public networks
- ❌ **NEVER** use for sensitive data

---

## PLAIN Authentication

**RFC 23**: Username/password authentication over the wire

### ⚠️ Security Warning

**PLAIN sends credentials in cleartext!** Always use PLAIN with transport-layer security:
- TLS/SSL tunnel (stunnel, nginx)
- VPN (WireGuard, OpenVPN)
- Private network only

### Client Configuration

```rust
use monocoque_core::SocketOptions;
use monocoque_zmtp::DealerSocket;

// Configure PLAIN credentials
let options = SocketOptions::new()
    .with_plain_credentials("myuser", "mypassword");

let mut socket = DealerSocket::with_options(options);
socket.connect("tcp://server:5555").await?;
```

### Server Configuration with Static Authentication

```rust
use monocoque_core::SocketOptions;
use monocoque_zmtp::RouterSocket;
use monocoque_zmtp::security::plain::StaticPlainHandler;
use std::collections::HashMap;

// Create a credential store
let mut credentials = HashMap::new();
credentials.insert("alice".to_string(), "secret123".to_string());
credentials.insert("bob".to_string(), "password456".to_string());

let handler = StaticPlainHandler::new(credentials);

let options = SocketOptions::new()
    .with_plain_server(true);

let mut socket = RouterSocket::with_options(options);
socket.bind("tcp://0.0.0.0:5555").await?;

// Use handler during accept
// Note: Full ZAP integration coming in Phase 8
```

### Custom Authentication Handler

Implement the `PlainAuthHandler` trait for advanced authentication:

```rust
use monocoque_zmtp::security::plain::PlainAuthHandler;
use async_trait::async_trait;

struct DatabaseAuthHandler {
    db_pool: sqlx::PgPool,
}

#[async_trait(?Send)]
impl PlainAuthHandler for DatabaseAuthHandler {
    async fn authenticate(
        &self,
        username: &str,
        password: &str,
        domain: &str,
        address: &str,
    ) -> Result<String, String> {
        // Query database for user
        let user = sqlx::query!(
            "SELECT password_hash FROM users WHERE username = $1",
            username
        )
        .fetch_optional(&self.db_pool)
        .await
        .map_err(|e| format!("Database error: {}", e))?;
        
        match user {
            Some(record) => {
                // Verify password hash
                if verify_password(password, &record.password_hash) {
                    Ok(format!("user_id={}", username))
                } else {
                    Err("Invalid password".to_string())
                }
            }
            None => Err("Unknown user".to_string()),
        }
    }
}
```

### PLAIN Best Practices

1. **Always use with TLS/VPN** - Credentials are sent in cleartext
2. **Use strong passwords** - Minimum 12 characters, mixed case, numbers, symbols
3. **Implement rate limiting** - Prevent brute-force attacks
4. **Log authentication failures** - Monitor for suspicious activity
5. **Rotate passwords regularly** - Enforce password expiration policies
6. **Use ZAP domain** - Segment different security contexts

### PLAIN with TLS (stunnel example)

```bash
# Server: stunnel.conf
[zeromq]
accept = 0.0.0.0:5556
connect = 127.0.0.1:5555
cert = /path/to/server-cert.pem
key = /path/to/server-key.pem

# Client connects to TLS port 5556
# Server binds to localhost 5555 with PLAIN
```

---

## CURVE Encryption

**RFC 26**: Elliptic curve cryptography (CurveZMQ) - **RECOMMENDED FOR PRODUCTION**

### Features
- ✅ **Public key authentication** - No shared secrets
- ✅ **Perfect forward secrecy** - Ephemeral keys per connection
- ✅ **Message encryption** - ChaCha20-Poly1305 AEAD
- ✅ **Message authentication** - Prevents tampering
- ✅ **No PKI required** - No certificate authorities

### Key Generation

```rust
use monocoque_zmtp::security::curve::CurveKeyPair;

// Generate server key pair
let server_keypair = CurveKeyPair::generate();
println!("Server public key: {}", hex::encode(server_keypair.public.as_bytes()));
println!("Server secret key: {}", hex::encode(server_keypair.secret.as_bytes()));

// Save keys securely (environment variables, key management system)
// NEVER commit secret keys to version control!
```

### Server Configuration

```rust
use monocoque_core::SocketOptions;
use monocoque_zmtp::RouterSocket;
use monocoque_zmtp::security::curve::CurveKeyPair;

// Load server keys from secure storage
let server_keypair = CurveKeyPair::generate(); // In production: load from env/KMS

let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(
        *server_keypair.public.as_bytes(),
        *server_keypair.secret.to_bytes()
    );

let mut socket = RouterSocket::with_options(options);
socket.bind("tcp://0.0.0.0:5555").await?;
```

### Client Configuration

```rust
use monocoque_core::SocketOptions;
use monocoque_zmtp::DealerSocket;
use monocoque_zmtp::security::curve::CurveKeyPair;

// Generate ephemeral client keys
let client_keypair = CurveKeyPair::generate();

// Server's public key (distributed out-of-band)
let server_public_key: [u8; 32] = hex::decode("ABC123...")
    .unwrap()
    .try_into()
    .unwrap();

let options = SocketOptions::new()
    .with_curve_keypair(
        *client_keypair.public.as_bytes(),
        *client_keypair.secret.to_bytes()
    )
    .with_curve_serverkey(server_public_key);

let mut socket = DealerSocket::with_options(options);
socket.connect("tcp://server:5555").await?;
```

### Key Management Best Practices

#### 1. **Server Key Storage**

```bash
# Environment variables (recommended for containers)
export CURVE_PUBLIC_KEY="your_public_key_hex"
export CURVE_SECRET_KEY="your_secret_key_hex"

# Load in application
let public_key = hex::decode(std::env::var("CURVE_PUBLIC_KEY")?)?;
let secret_key = hex::decode(std::env::var("CURVE_SECRET_KEY")?)?;
```

#### 2. **Key Distribution**

- **Server public key**: Distribute via configuration management, DNS TXT records, or HTTPS endpoint
- **Client keys**: Generate ephemeral keys per connection (recommended) or per client instance
- **Never share secret keys** between different services

#### 3. **Key Rotation**

```rust
// Graceful key rotation strategy:
// 1. Generate new server keypair
// 2. Configure clients with new server public key
// 3. Server accepts both old and new keys during transition period
// 4. Retire old server keypair after all clients migrated

// Example: Accept multiple server keys (requires custom implementation)
struct MultiKeyServer {
    current_keypair: CurveKeyPair,
    legacy_keypair: Option<CurveKeyPair>,
}
```

#### 4. **Secret Key Protection**

- ✅ Use environment variables or secrets management (Vault, AWS Secrets Manager)
- ✅ Restrict file permissions (`chmod 600`)
- ✅ Encrypt at rest using OS keychain or HSM
- ❌ Never log secret keys
- ❌ Never commit to version control
- ❌ Never send over unencrypted channels

### CURVE Performance Considerations

- **Handshake overhead**: ~3ms for key exchange (one-time per connection)
- **Message overhead**: ~32 bytes per message (MAC + nonce)
- **CPU usage**: ChaCha20-Poly1305 is highly optimized (minimal overhead on modern CPUs)
- **Throughput**: Typically 90-95% of NULL mechanism throughput

---

## ZAP Protocol

**RFC 27**: ZeroMQ Authentication Protocol - extensible authentication framework

### Architecture

```
┌─────────────┐                    ┌──────────────┐
│   Client    │                    │   Server     │
│   Socket    │──── ZMTP handshake ────│   Socket     │
└─────────────┘                    └──────┬───────┘
                                          │
                                          │ ZAP Request
                                          ▼
                                   ┌──────────────┐
                                   │ ZAP Handler  │
                                   │ (DEALER/REP) │
                                   └──────────────┘
                                   inproc://zeromq.zap.01
```

### ZAP Request/Response Flow

1. **Client initiates connection** → Server socket receives
2. **Server extracts credentials** from ZMTP handshake (PLAIN or CURVE)
3. **Server sends ZAP request** to `inproc://zeromq.zap.01`
4. **ZAP handler processes** authentication logic
5. **Handler sends ZAP response** with status code (200/300/400/500)
6. **Server allows/denies** connection based on response

### ZAP Request Format

```rust
use monocoque_zmtp::security::zap::ZapRequest;

// Example PLAIN ZAP request
let request = ZapRequest {
    version: "1.0".to_string(),
    request_id: "1".to_string(),
    domain: "global".to_string(),
    address: "192.168.1.100".to_string(),
    identity: vec![],
    mechanism: Mechanism::Plain,
    credentials: vec![
        Bytes::from("username"),
        Bytes::from("password"),
    ],
};

// Example CURVE ZAP request
let request = ZapRequest {
    version: "1.0".to_string(),
    request_id: "2".to_string(),
    domain: "global".to_string(),
    address: "192.168.1.101".to_string(),
    identity: vec![],
    mechanism: Mechanism::Curve,
    credentials: vec![
        Bytes::copy_from_slice(&client_public_key),
    ],
};
```

### ZAP Response Format

```rust
use monocoque_zmtp::security::zap::ZapResponse;

// Success response
let response = ZapResponse {
    version: "1.0".to_string(),
    request_id: "1".to_string(),
    status_code: "200".to_string(),
    status_text: "OK".to_string(),
    user_id: "alice".to_string(),
    metadata: vec![
        (Bytes::from("role"), Bytes::from("admin")),
        (Bytes::from("department"), Bytes::from("engineering")),
    ],
};

// Failure response
let response = ZapResponse {
    version: "1.0".to_string(),
    request_id: "1".to_string(),
    status_code: "400".to_string(),
    status_text: "Invalid credentials".to_string(),
    user_id: String::new(),
    metadata: vec![],
};
```

### ZAP Status Codes

| Code | Meaning | Action |
|------|---------|--------|
| **200** | Success | Allow connection |
| **300** | Temporary failure | Retry later (not implemented) |
| **400** | Authentication failed | Reject connection |
| **500** | Internal error | Reject connection |

### ZAP Domains

Use domains to segment different security contexts:

```rust
let options = SocketOptions::new()
    .with_plain_server(true)
    .with_zap_domain("backend"); // Separate from "frontend" domain

// Handler can apply different policies per domain
if domain == "backend" {
    // Require strong authentication
} else if domain == "frontend" {
    // Allow guest access
}
```

### Custom ZAP Handler Example

```rust
use monocoque_zmtp::DealerSocket;
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};
use bytes::Bytes;

async fn zap_handler() -> io::Result<()> {
    let mut socket = DealerSocket::new();
    socket.bind("inproc://zeromq.zap.01").await?;
    
    loop {
        let msg = socket.recv().await?;
        if msg.is_none() {
            continue;
        }
        
        let request = ZapRequest::decode(&msg.unwrap())?;
        
        // Custom authentication logic
        let response = authenticate_request(&request).await;
        
        let frames = response.encode();
        socket.send(frames).await?;
    }
}

async fn authenticate_request(req: &ZapRequest) -> ZapResponse {
    match req.mechanism {
        Mechanism::Plain => {
            // Check username/password against database
            validate_plain_credentials(req).await
        }
        Mechanism::Curve => {
            // Check public key against whitelist
            validate_curve_key(req).await
        }
        _ => ZapResponse::unauthorized("Unsupported mechanism"),
    }
}
```

---

## Best Practices

### 1. **Choose the Right Mechanism**

| Scenario | Recommended |
|----------|-------------|
| Development/testing | NULL |
| Trusted private network + need auth | PLAIN over VPN/TLS |
| Production over internet | **CURVE** |
| Regulated/compliance | **CURVE** + audit logging |
| High-security | CURVE + mutual authentication + ZAP |

### 2. **Defense in Depth**

```
┌─────────────────────────────────────┐
│ Network Layer (Firewall, VPN)      │
├─────────────────────────────────────┤
│ Transport Layer (TLS - optional)   │
├─────────────────────────────────────┤
│ ZMTP Security (CURVE/PLAIN)         │
├─────────────────────────────────────┤
│ Application Auth (ZAP handlers)    │
├─────────────────────────────────────┤
│ Authorization (per-message checks) │
└─────────────────────────────────────┘
```

### 3. **Monitoring and Logging**

```rust
// Log authentication events
impl PlainAuthHandler for MyHandler {
    async fn authenticate(&self, username: &str, ...) -> Result<String, String> {
        match self.validate(username, password).await {
            Ok(user_id) => {
                log::info!("Authentication success: user={} from={}", username, address);
                Ok(user_id)
            }
            Err(e) => {
                log::warn!("Authentication failed: user={} from={} error={}", 
                           username, address, e);
                Err(e)
            }
        }
    }
}
```

### 4. **Error Handling**

```rust
// Don't leak information in error messages
match socket.connect("tcp://server:5555").await {
    Ok(_) => {},
    Err(e) => {
        // Log detailed error internally
        log::error!("Connection failed: {}", e);
        
        // Return generic error to user
        return Err("Authentication failed".into());
    }
}
```

### 5. **Rate Limiting**

```rust
use std::collections::HashMap;
use std::time::{Instant, Duration};

struct RateLimiter {
    attempts: HashMap<String, Vec<Instant>>,
    max_attempts: usize,
    window: Duration,
}

impl RateLimiter {
    fn check(&mut self, address: &str) -> bool {
        let now = Instant::now();
        let attempts = self.attempts.entry(address.to_string())
            .or_insert_with(Vec::new);
        
        // Remove old attempts
        attempts.retain(|&time| now.duration_since(time) < self.window);
        
        if attempts.len() >= self.max_attempts {
            return false; // Rate limit exceeded
        }
        
        attempts.push(now);
        true
    }
}
```

---

## Migration from libzmq

### PLAIN Migration

```c
// libzmq (C)
zmq_setsockopt(socket, ZMQ_PLAIN_SERVER, &enabled, sizeof(enabled));
zmq_setsockopt(socket, ZMQ_PLAIN_USERNAME, "admin", 5);
zmq_setsockopt(socket, ZMQ_PLAIN_PASSWORD, "secret", 6);
```

```rust
// monocoque (Rust)
let options = SocketOptions::new()
    .with_plain_server(true)
    .with_plain_credentials("admin", "secret");
```

### CURVE Migration

```c
// libzmq (C)
char server_public[41];
char server_secret[41];
char client_public[41];
char client_secret[41];

// Generate keys
zmq_curve_keypair(server_public, server_secret);
zmq_curve_keypair(client_public, client_secret);

// Server
zmq_setsockopt(socket, ZMQ_CURVE_SERVER, &enabled, sizeof(enabled));
zmq_setsockopt(socket, ZMQ_CURVE_SECRETKEY, server_secret, 40);

// Client
zmq_setsockopt(socket, ZMQ_CURVE_PUBLICKEY, client_public, 40);
zmq_setsockopt(socket, ZMQ_CURVE_SECRETKEY, client_secret, 40);
zmq_setsockopt(socket, ZMQ_CURVE_SERVERKEY, server_public, 40);
```

```rust
// monocoque (Rust)
use monocoque_zmtp::security::curve::CurveKeyPair;

// Generate keys
let server_keypair = CurveKeyPair::generate();
let client_keypair = CurveKeyPair::generate();

// Server
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(
        *server_keypair.public.as_bytes(),
        *server_keypair.secret.to_bytes()
    );

// Client
let options = SocketOptions::new()
    .with_curve_keypair(
        *client_keypair.public.as_bytes(),
        *client_keypair.secret.to_bytes()
    )
    .with_curve_serverkey(*server_keypair.public.as_bytes());
```

### ZAP Migration

libzmq uses a dedicated thread for ZAP, monocoque uses async tasks:

```rust
// libzmq equivalent: start ZAP thread
// monocoque: spawn async task
compio::runtime::spawn(async {
    zap_handler().await.unwrap();
});
```

---

## Security Checklist

### Development
- [ ] Use NULL mechanism only on localhost
- [ ] Never commit secret keys to version control
- [ ] Document security requirements in README

### Staging/Testing
- [ ] Test PLAIN with TLS tunnel
- [ ] Verify CURVE key rotation procedures
- [ ] Load test authentication under high connection rates
- [ ] Test authentication failure scenarios

### Production
- [ ] **Use CURVE** for all public-facing endpoints
- [ ] Store secret keys in secrets management system
- [ ] Implement ZAP handlers with audit logging
- [ ] Set up monitoring for authentication failures
- [ ] Configure rate limiting and IP blocking
- [ ] Enable TLS for additional defense layer
- [ ] Document incident response procedures
- [ ] Regular security audits and penetration testing

---

## Troubleshooting

### "Handshake timeout" errors

**Cause**: Mismatched security mechanisms

```rust
// Server expects CURVE
let server_opts = SocketOptions::new().with_curve_server(true);

// Client uses NULL
let client_opts = SocketOptions::new(); // ❌ Mismatch!

// Fix: Client must use CURVE
let client_opts = SocketOptions::new()
    .with_curve_keypair(/* ... */)
    .with_curve_serverkey(/* ... */);
```

### "Authentication failed" with PLAIN

**Check**:
1. Username/password are correct (case-sensitive!)
2. PlainAuthHandler is properly configured
3. ZAP handler is running (if using custom handler)
4. Network allows connection (no firewall blocking)

### CURVE key format errors

```rust
// ❌ Wrong: Using Z85 encoding
let key = "z85_encoded_key";

// ✅ Correct: Raw 32-byte array
let key: [u8; 32] = hex::decode("hex_key")?.try_into()?;
```

### Performance degradation

- Check if using PLAIN without TLS (should be fast)
- CURVE adds ~5-10% overhead - this is normal
- Profile ZAP handler - authentication logic may be slow
- Consider connection pooling to amortize handshake cost

---

## Additional Resources

- [RFC 23 - ZMTP 3.1](https://rfc.zeromq.org/spec/23/)
- [RFC 26 - CurveZMQ](https://rfc.zeromq.org/spec/26/)
- [RFC 27 - ZAP Protocol](https://rfc.zeromq.org/spec/27/)
- [CurveCP: Usable security for the Internet](https://curvecp.org/)
- [ZeroMQ Security Whitepaper](https://zeromq.org/whitepapers/)

---

**Questions or Issues?**

File an issue at: https://github.com/yourusername/monocoque/issues

**Security Vulnerabilities?**

Email: security@yourproject.org (Do not file public issues!)
