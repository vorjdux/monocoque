# Security Guide

Monocoque supports three security mechanisms: NULL (no security), PLAIN (username/password), and CURVE (public-key encryption). Security is negotiated during the ZMTP handshake - once a connection is established, all messages on it carry that security context.

Use NULL only for localhost or development. Use PLAIN only over an encrypted transport (TLS, VPN). Use CURVE for anything public-facing.

## PLAIN Authentication

PLAIN sends credentials in cleartext, so it must run over TLS or a VPN in production. It's fine for internal services on a trusted network where you want simple username/password access control.

```rust
// Client
let options = SocketOptions::new()
    .with_plain_credentials("alice", "secret");

let mut socket = DealerSocket::with_options(options);
socket.connect("tcp://server:5555").await?;

// Server with a static credential store
use monocoque_zmtp::security::plain::StaticPlainHandler;

let mut handler = StaticPlainHandler::new();
handler.add_user("alice", "secret");
handler.add_user("bob", "hunter2");

let options = SocketOptions::new().with_plain_server(true);
let mut socket = RouterSocket::with_options(options);
socket.bind("tcp://0.0.0.0:5555").await?;
```

For dynamic credentials (database lookup, LDAP, etc.), implement `PlainAuthHandler`:

```rust
use monocoque_zmtp::security::plain::PlainAuthHandler;

struct DbAuthHandler { pool: sqlx::PgPool }

#[async_trait::async_trait(?Send)]
impl PlainAuthHandler for DbAuthHandler {
    async fn authenticate(
        &self, username: &str, password: &str,
        _domain: &str, _address: &str,
    ) -> Result<String, String> {
        let row = sqlx::query!("SELECT password_hash FROM users WHERE username = $1", username)
            .fetch_optional(&self.pool).await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "unknown user".to_string())?;

        if verify_password(password, &row.password_hash) {
            Ok(username.to_string())
        } else {
            Err("invalid password".to_string())
        }
    }
}
```

See `examples/plain_auth_demo.rs` for a runnable example.

## CURVE Encryption

CURVE is what you should use in production. It provides mutual authentication and message encryption (ChaCha20-Poly1305) with no shared secrets and no certificate authority. The server has a long-term keypair; clients get the server's public key out-of-band and generate ephemeral keys per connection.

```rust
use monocoque_zmtp::security::curve::CurveKeyPair;

// Generate a server keypair once; store the secret key securely.
let server_kp = CurveKeyPair::generate();
// Distribute server_kp.public to clients (config file, environment variable, etc.)

// Server
let server_opts = SocketOptions::default()
    .with_curve_server(true)
    .with_curve_keypair(*server_kp.public.as_bytes(), *server_kp.secret.as_bytes());

let (_listener, mut server) = RouterSocket::bind_with_options("0.0.0.0:5555", server_opts).await?;

// Client
let client_kp = CurveKeyPair::generate(); // ephemeral is fine

let server_pubkey: [u8; 32] = hex::decode(std::env::var("SERVER_PUBLIC_KEY")?)?.try_into()?;

let client_opts = SocketOptions::default()
    .with_curve_keypair(*client_kp.public.as_bytes(), *client_kp.secret.as_bytes())
    .with_curve_serverkey(server_pubkey);

let mut client = DealerSocket::connect_with_options("server:5555", client_opts).await?;
```

The ZMTP handshake completes automatically on connect/accept. Handshake overhead is roughly 3 ms; per-message overhead is around 32 bytes (MAC + nonce). Throughput is typically 90-95% of NULL.

Store the server secret key in an environment variable or a secrets manager. Never log it, never commit it.

See `examples/curve_demo.rs` for a full working example.

## ZAP Authentication

Without ZAP, any client that knows the server's public key can connect over CURVE, and any client that knows a valid credential can connect over PLAIN. ZAP lets you plug in custom authorization logic that runs during the handshake.

A ZAP handler is an async task that receives authentication requests over `inproc://zeromq.zap.01` and responds with allow/deny decisions.

```rust
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};
use std::collections::HashSet;
use std::sync::Arc;

struct AllowlistHandler {
    allowed_keys: HashSet<[u8; 32]>,
}

#[async_trait::async_trait(?Send)]
impl ZapHandler for AllowlistHandler {
    async fn authenticate(&self, req: &ZapRequest) -> ZapResponse {
        // For CURVE, credentials[0] is the client's 32-byte public key.
        let allowed = req.credentials.first()
            .and_then(|k| k.as_ref().try_into().ok())
            .map(|key: [u8; 32]| self.allowed_keys.contains(&key))
            .unwrap_or(false);

        if allowed {
            ZapResponse::success(req.request_id.clone(), "client".to_string())
        } else {
            ZapResponse::failure(req.request_id.clone(), "not authorized".to_string())
        }
    }
}

// Call this before binding your server socket.
spawn_zap_server(Arc::new(AllowlistHandler { allowed_keys: load_allowlist() }))?;
```

ZAP adds one inproc round-trip to connection setup (typically 50-500 µs depending on handler complexity). It does not affect per-message performance.

Domains let you apply different policies on the same server:

```rust
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_zap_domain("backend"); // handler receives this in req.domain
```

See `examples/zap_server_demo.rs` and `examples/authenticated_req_rep.rs` for full examples. The ZAP handler details are in [ZAP_INTEGRATION_GUIDE.md](ZAP_INTEGRATION_GUIDE.md).

## Troubleshooting

**Handshake timeout** - client and server are using different security mechanisms. Both sides must agree: if the server uses CURVE, the client must provide a keypair and the server's public key.

**Authentication always fails with PLAIN** - credentials are case-sensitive. Check that the handler is configured before the server socket starts accepting.

**CURVE key format errors** - monocoque uses raw 32-byte arrays, not Z85 encoding. `hex::decode(...).try_into().unwrap()` is the usual conversion from a hex string.

**ZAP request timeout** - the ZAP handler must be started before the server socket. See [ZAP_INTEGRATION_GUIDE.md](ZAP_INTEGRATION_GUIDE.md) for startup ordering.
