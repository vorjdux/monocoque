# Security Guide

Monocoque supports three security mechanisms: NULL (no security), PLAIN (username/password), and CURVE (public-key encryption). Security is negotiated during the ZMTP handshake - once a connection is established, all messages on it carry that security context.

Use NULL only for localhost or development. Use PLAIN only over an encrypted transport (TLS, VPN). Use CURVE for anything public-facing.

Everything on this page comes from the `monocoque-rs` crate with the `zmq` feature: the key, credential and ZAP types are in `monocoque::zmq::security`.

Who gets in is decided by a ZAP handler, a task in the server's process (see [ZAP Authentication](#zap-authentication)). The three mechanisms use it differently:

| Mechanism | When the handler is asked | With no handler running |
|---|---|---|
| NULL | never, even with a ZAP domain set | every peer is admitted |
| PLAIN | for every client | every client is refused |
| CURVE | for every client, when a handler is running or the socket has a ZAP domain | with a ZAP domain: every client is refused. Without one: any client key is admitted (encryption only), and a warning is logged |

## PLAIN Authentication

PLAIN sends credentials in cleartext, so it must run over TLS or a VPN in production. It's fine for internal services on a trusted network where you want simple username/password access control. A PLAIN server checks every client through ZAP, so start the handler before binding; without one, every client is refused.

```rust
// Client
let options = SocketOptions::new()
    .with_plain_credentials("alice", "secret");

let mut socket = DealerSocket::with_options(options);
socket.connect("tcp://server:5555").await?;

// Server with a static credential store, checked through ZAP
use monocoque::zmq::security::plain::StaticPlainHandler;
use monocoque::zmq::security::zap_handler::start_default_zap_server;
use std::sync::Arc;

let mut handler = StaticPlainHandler::new();
handler.add_user("alice", "secret");
handler.add_user("bob", "hunter2");
start_default_zap_server(Arc::new(handler), false)?;

let options = SocketOptions::new().with_plain_server(true);

// Every peer that connects to this endpoint runs the PLAIN handshake.
let mut socket = RouterSocket::with_options(options);
socket.bind("tcp://0.0.0.0:5555").await?;
```

For dynamic credentials (database lookup, LDAP, etc.), implement `PlainAuthHandler`. It uses native `async fn`, so it cannot be a `dyn` object; pass the concrete type, for example `Arc<DbAuthHandler>`:

```rust
use monocoque::zmq::security::plain::PlainAuthHandler;

struct DbAuthHandler { pool: sqlx::PgPool }

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

CURVE is what you should use in production. It encrypts every message (XSalsa20-Poly1305, via NaCl `crypto_box`) and proves the server's identity to the client, with no shared secrets and no certificate authority. The server has a long-term keypair; clients get the server's public key out-of-band and generate ephemeral keys per connection.

CURVE alone does not decide which clients may connect: the server learns each client's public key, and a ZAP handler has to check it against a list (see [ZAP Authentication](#zap-authentication)). Without a handler any client that knows the server's public key gets in.

```rust
use monocoque::zmq::security::curve::{CurveKeyPair, CurveSecretKey};

// Generate the server's key pair once and store the secret bytes securely:
//     let pair = CurveKeyPair::generate();
//     save(pair.secret.to_bytes());
// Later runs load the secret; the public key is derived from it. Distribute
// the public key to clients (config file, environment variable, etc.)
let server_secret: [u8; 32] = load_server_secret();
let server_public = *CurveSecretKey::from_bytes(server_secret).public_key().as_bytes();

// Server
let server_opts = SocketOptions::default()
    .with_curve_server(true)
    .with_curve_keypair(server_public, server_secret);

let mut server = RouterSocket::with_options(server_opts);
server.bind("tcp://0.0.0.0:5555").await?;

// Client. A fresh key per run is fine unless the server keeps an allowlist
// of client keys, in which case the client's key has to be stored too.
let client = CurveKeyPair::generate();

let server_pubkey: [u8; 32] = hex::decode(std::env::var("SERVER_PUBLIC_KEY")?)?.try_into()?;

let client_opts = SocketOptions::default()
    .with_curve_keypair(*client.public.as_bytes(), client.secret.to_bytes())
    .with_curve_serverkey(server_pubkey);

let mut client = DealerSocket::with_options(client_opts);
client.connect("tcp://server:5555").await?;
```

The ZMTP handshake runs automatically on every connection a socket makes or accepts. CURVE follows RFC 26 and interoperates with libzmq in both roles, and custom properties set with `with_metadata` travel inside the encrypted handshake. Handshake overhead is roughly 3 ms; per-message overhead is around 32 bytes (MAC + nonce). Throughput is typically 90-95% of NULL.

Store the server secret key in an environment variable or a secrets manager. Never log it, never commit it.

See `examples/curve_demo.rs` for a full working example.

## ZAP Authentication

ZAP is where access is decided. Without a handler, any client that knows the server's public key can connect over CURVE, and no client at all can connect over PLAIN. A handler runs your own check during the handshake, before the peer can send or receive a message.

A ZAP handler is an async task that receives authentication requests over `inproc://zeromq.zap.01` and responds with allow/deny decisions. There is one per process, and it is asked on behalf of every PLAIN and CURVE server socket in it. A NULL socket never asks it, so a ZAP domain on a NULL socket gives no protection. `ZapHandler` uses native `async fn`, so store the concrete handler type (`Arc<AllowlistHandler>`), not a `dyn ZapHandler`.

```rust
use monocoque::zmq::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque::zmq::security::zap::{ZapRequest, ZapResponse};
use std::collections::HashSet;
use std::sync::Arc;

struct AllowlistHandler {
    allowed_keys: HashSet<[u8; 32]>,
}

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

For the common cases there is `DefaultZapHandler`: it checks PLAIN credentials through a `PlainAuthHandler`, and `with_curve_whitelist(keys)` restricts CURVE to a list of client public keys.

ZAP adds one inproc round-trip to connection setup (typically 50-500 µs depending on handler complexity). It does not affect per-message performance.

Set a ZAP domain on a CURVE or PLAIN server socket. The handler receives it in `req.domain`, so one handler can apply different policies to different sockets, and a CURVE server with a domain refuses every client if the handler is not running, instead of falling back to admitting any key:

```rust
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_curve_keypair(server_public, server_secret)
    .with_zap_domain("backend");
```

`req.address` is the peer's IP address as a string, for allow and deny lists by address. It is empty for a peer on `ipc://`.

See `examples/zap_server_demo.rs` for a ZAP handler and `examples/authenticated_req_rep.rs` for a client and server using PLAIN with ZAP. The ZAP handler details are in [ZAP_INTEGRATION_GUIDE.md](ZAP_INTEGRATION_GUIDE.md).

### Repeated failures

A server slows down an address that keeps failing authentication. After two failures in a row from one IP, each new connection from it waits before its handshake starts: 100 ms, doubling with each further failure, up to 10 s. A successful login clears the address, and one that stays quiet for a minute is forgotten. IPC peers have no address and are not delayed. This bounds online password guessing per address; it is not a substitute for rate limiting at the network edge.

## What each mechanism protects against

| Threat | NULL | PLAIN | CURVE |
|---|---|---|---|
| Eavesdropping | exposed | exposed | encrypted |
| Credential theft | n/a | credentials in cleartext | no credentials on the wire |
| Man in the middle | exposed | exposed | the client pins the server's key |
| Replay | exposed | exposed | per-connection nonces |
| Unauthorized connect | open | checked by ZAP | checked by ZAP when a handler lists the allowed keys |
| Denial of service | open | open | open; the handshake costs the server CPU |

None of them protects against denial of service; limit connection rates at the network edge.

Suggested setups:

- Services on one host: NULL over `ipc://` or loopback, with file permissions or network isolation.
- Traffic inside a cluster: CURVE with a pinned server public key.
- Public endpoints: CURVE, a ZAP handler with an allowlist of client keys, a ZAP domain on the socket, and rate limiting in front.

## Troubleshooting

**Handshake timeout** - client and server are using different security mechanisms. Both sides must agree: if the server uses CURVE, the client must provide a keypair and the server's public key.

**Authentication always fails with PLAIN** - credentials are case-sensitive. Check that the handler is configured before the server socket starts accepting.

**CURVE key format errors** - monocoque uses raw 32-byte arrays, not Z85 encoding. `hex::decode(...).try_into().unwrap()` is the usual conversion from a hex string.

**CURVE handshake fails at once on both sides** - `with_curve_keypair` was given a public key that does not belong to the secret key. Derive it with `CurveSecretKey::from_bytes(secret).public_key()`.

**A CURVE client is refused after a ZAP handler was added** - a running handler is asked about every CURVE client. `DefaultZapHandler::new(handler, false)` refuses CURVE; pass `true`, with `with_curve_whitelist` to name the allowed keys.

**ZAP request timeout** - the ZAP handler must be started before the server socket. See [ZAP_INTEGRATION_GUIDE.md](ZAP_INTEGRATION_GUIDE.md) for startup ordering.
