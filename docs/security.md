# Security Model

monocoque implements the same security mechanisms as libzmq: NULL (no auth),
PLAIN (cleartext credentials), and CURVE (public-key encryption + authentication).
All are negotiated during the ZMTP handshake before any application data is exchanged.

---

## NULL (default)

NULL is the default. Any peer can connect with no authentication or encryption.
Use only on loopback or within a fully-trusted network.

---

## PLAIN

PLAIN sends a username and password in cleartext. It provides *authentication*
but **no encryption**. Only use PLAIN over loopback, a VPN, or inside CURVE.

### Server side

```rust
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::security::plain::StaticPlainHandler;
use monocoque_zmtp::security::zap_handler::{DefaultZapHandler, spawn_zap_server};
use std::sync::Arc;

// Build a credential store
let mut handler = StaticPlainHandler::new();
handler.add_user("alice", "s3cr3t");
handler.add_user("bob",   "hunter2");

// Spawn the ZAP server on inproc://zeromq.zap.01
// spawn_zap_server is synchronous  -  it registers a background task, no .await needed.
let zap = DefaultZapHandler::new(Arc::new(handler), false);
spawn_zap_server(Arc::new(zap))?;

// Configure the server socket
let opts = SocketOptions::default()
    .with_plain_server(true)
    .with_zap_domain("global");
```

### Client side

```rust
// with_plain_credentials takes (username, password) as a single combined call
let opts = SocketOptions::default()
    .with_plain_credentials("alice", "s3cr3t");
```

### Security warning

PLAIN offers zero confidentiality. Anyone who can observe the TCP stream can
read the credentials. Combine with CURVE or run over a TLS tunnel.

---

## CURVE

CURVE provides both *encryption* and *mutual authentication* using Curve25519
public-key cryptography. Messages cannot be read or forged by a third party.

### Key generation

```rust
use monocoque_zmtp::security::CurveKeyPair;

let server_keys = CurveKeyPair::generate();
let client_keys = CurveKeyPair::generate();

// Fields are .public (CurvePublicKey) and .secret (CurveSecretKey).
// Persist keys securely  -  lose the secret key and the session cannot be
// decrypted; expose it and an attacker can impersonate you.
println!("server public:  {:?}", server_keys.public);
println!("server secret:  {:?}", server_keys.secret); // keep private!
```

### Server side

```rust
// with_curve_keypair takes (public_key_bytes, secret_key_bytes) together.
let opts = SocketOptions::default()
    .with_curve_server(true)
    .with_curve_keypair(
        server_keys.public.as_bytes(),
        server_keys.secret.as_bytes(),
    );
```

### Client side

The client must know the server's *public* key before connecting (shared out-of-band).

```rust
let opts = SocketOptions::default()
    .with_curve_keypair(
        client_keys.public.as_bytes(),
        client_keys.secret.as_bytes(),
    )
    .with_curve_serverkey(server_keys.public.as_bytes()); // server pubkey
```

### Key distribution

Never transmit secret keys over the network. Distribute server public keys:
- Embedded in your application binary (pinned key)
- Via a secure PKI / key-management service
- Using `zmq_z85_encode` / hex format for human-readable configs

### Key rotation

1. Generate a new server key pair.
2. Deploy the new server, accepting both old and new client keys via a ZAP handler.
3. Roll clients to the new server public key.
4. Retire the old server key.

---

## ZAP (ZeroMQ Authentication Protocol)

ZAP lets you run custom authentication logic in a separate thread/task.
monocoque's ZAP handler runs on `inproc://zeromq.zap.01`.

### Default behaviour

If **no ZAP handler is registered**, authentication for PLAIN and CURVE
mechanisms is **denied** (the connection is rejected). NULL mechanism
connections are always accepted regardless of ZAP.

### Custom handler

```rust
use monocoque_zmtp::security::zap_handler::ZapHandler;
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};

struct MyHandler;

#[async_trait::async_trait(?Send)]
impl ZapHandler for MyHandler {
    async fn authenticate(&self, req: &ZapRequest) -> ZapResponse {
        // Reject connections from a specific IP range
        if req.address.starts_with("10.0.0.") {
            return ZapResponse::failure(req.request_id.clone(), "IP blocked");
        }
        ZapResponse::success(req.request_id.clone(), req.address.clone())
    }
}
```

### IP filtering

The `ZapRequest.address` field contains the peer's IP address as a string.
Implement allowlist/denylist logic in your `ZapHandler::authenticate` method.

---

## Threat model

| Threat | NULL | PLAIN | CURVE |
|--------|------|-------|-------|
| Eavesdropping | ❌ exposed | ❌ exposed | ✅ encrypted |
| Credential theft | N/A | ❌ in cleartext | ✅ no credentials on wire |
| Man-in-the-middle | ❌ | ❌ | ✅ server key pinned |
| Replay attack | ❌ | ❌ | ✅ per-session nonces |
| Unauthorized connect | ❌ | ✅ (if ZAP configured) | ✅ |
| Denial of service | ❌ | ❌ | partial (handshake cost) |

CURVE does **not** protect against DoS; rate-limit connections at the network layer.

---

## Recommended configuration

- **Internal services on the same host**: NULL + network-level isolation (namespaces, firewall)
- **Internal cluster traffic**: CURVE with a shared server public key
- **Public-facing endpoints**: CURVE + ZAP with IP allowlist + rate limiting
- **Legacy interop with non-ZMTP peers**: STREAM socket + application-level auth
