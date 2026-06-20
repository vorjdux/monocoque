# ZAP Integration

ZAP (ZeroMQ Authentication Protocol) lets you plug custom authorization logic into the ZMTP handshake. When a client connects, monocoque sends an authentication request to a handler running on `inproc://zeromq.zap.01`. The handler responds with allow or deny, and the connection is accepted or rejected before any application messages are exchanged.

This guide covers implementing and wiring up a ZAP handler. For how PLAIN and CURVE are configured on the socket side, see [SECURITY_GUIDE.md](SECURITY_GUIDE.md).

## Setting Up a Handler

Implement the `ZapHandler` trait and spawn it before binding your server socket:

```rust
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse};
use std::sync::Arc;

struct MyHandler;

#[async_trait::async_trait(?Send)]
impl ZapHandler for MyHandler {
    async fn authenticate(&self, req: &ZapRequest) -> ZapResponse {
        // req.mechanism is "PLAIN" or "CURVE"
        // req.address  is the client's IP address
        // req.domain   is the ZAP domain set on the server socket
        //
        // For PLAIN: req.credentials = [username_bytes, password_bytes]
        // For CURVE: req.credentials = [client_public_key_32_bytes]

        let authorized = check_authorization(req);

        if authorized {
            ZapResponse::success(req.request_id.clone(), "user-id".to_string())
        } else {
            ZapResponse::failure(req.request_id.clone(), "not authorized".to_string())
        }
    }
}

// Start the handler before binding.
spawn_zap_server(Arc::new(MyHandler))?;
```

`spawn_zap_server` binds to `inproc://zeromq.zap.01` and runs the handler in a background task. The handler must be running before any authenticated connections arrive — if the ZAP socket isn't bound, the server will time out waiting for a response.

## Accepting and Rejecting Connections

`ZapResponse::success` accepts the connection. The second argument is a user ID string that monocoque makes available to the application layer (useful for logging and per-connection authorization).

`ZapResponse::failure` rejects the connection. The second argument is a reason string logged internally; it is not sent to the client.

For CURVE, the client's 32-byte public key is in `req.credentials[0]`. A typical allowlist check:

```rust
async fn authenticate(&self, req: &ZapRequest) -> ZapResponse {
    let client_key: Option<[u8; 32]> = req.credentials.first()
        .and_then(|b| b.as_ref().try_into().ok());

    match client_key {
        Some(key) if self.allowlist.contains(&key) => {
            ZapResponse::success(req.request_id.clone(), hex::encode(key))
        }
        _ => ZapResponse::failure(req.request_id.clone(), "key not in allowlist".to_string()),
    }
}
```

For PLAIN, credentials are the raw username and password bytes:

```rust
let username = std::str::from_utf8(&req.credentials[0]).unwrap_or("");
let password = std::str::from_utf8(&req.credentials[1]).unwrap_or("");
```

## Domains

Set a ZAP domain on the server socket to pass context to the handler:

```rust
let options = SocketOptions::new()
    .with_curve_server(true)
    .with_zap_domain("payments");
```

The handler receives this in `req.domain`. One handler can serve multiple sockets with different policies:

```rust
let policy = match req.domain.as_str() {
    "payments" => require_strong_auth(req),
    "metrics"  => allow_all(req),
    _          => ZapResponse::failure(req.request_id.clone(), "unknown domain".to_string()),
};
```

## Examples

See `examples/zap_server_demo.rs` for a standalone ZAP handler, and `examples/authenticated_req_rep.rs` for a complete client/server pair using CURVE + ZAP together.
