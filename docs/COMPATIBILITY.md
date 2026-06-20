# Protocol Compatibility

Monocoque implements ZMTP 3.x and is wire-compatible with all ZeroMQ 4.x releases (minimum 4.1). It sends ZMTP 3.0 greetings for backward compatibility and accepts any 3.x peer, so it interoperates with libzmq 4.1 through 4.4 without configuration.

When both peers advertise ZMTP 3.1, Monocoque enables heartbeating via PING/PONG (RFC 37). With a 3.0 peer, heartbeating is disabled automatically.

## Supported socket types

All standard socket types are implemented: DEALER, ROUTER, PUB, SUB, REQ, REP, PUSH, PULL, XPUB, XSUB, PAIR.

## Supported protocol features

- NULL security mechanism (no authentication, default)
- PLAIN security (username/password)
- CURVE security (CurveZMQ public-key encryption)
- ZAP authentication protocol
- ZMTP framing — 1-byte and 8-byte length frames
- Multipart messages (MORE flag)
- Command frames — READY, PING, PONG
- Heartbeating — ZMTP 3.1 PING/PONG (RFC 37)
- ROUTER fair-queuing and routing IDs
- PUB/SUB topic filtering
- Automatic reconnection with exponential backoff
- Full socket options API equivalent to `zmq_setsockopt`

## Behavioral differences from libzmq

Monocoque is async-first. There is no blocking `send`/`recv` API; all socket operations are `async fn`. This is a design choice, not a limitation — it eliminates the need for thread-per-connection and generally improves performance under load.

Socket types are enforced at compile time rather than at runtime. Attempting an invalid pattern (e.g., connecting PUB to PUB) is a type error, not a runtime failure.

The wire protocol is byte-for-byte compatible. Framing, handshake, and READY command formats match RFC 23/ZMTP exactly, so Monocoque peers are indistinguishable from libzmq peers on the wire.

## Running interoperability tests

The test suite includes interop tests against a live libzmq installation:

```bash
# Install libzmq
sudo apt install libzmq3-dev   # Ubuntu/Debian
brew install zeromq            # macOS

# Run interop tests
cargo test --package monocoque --features zmq --test interop_pair
cargo test --package monocoque --features zmq --test interop_router
cargo test --package monocoque --features zmq --test interop_pubsub
cargo test --package monocoque --features zmq --test interop_load_balance
```

## References

- [RFC 23/ZMTP](https://rfc.zeromq.org/spec:23/ZMTP/) — ZMTP 3.0 specification
- [RFC 37/ZMTP](https://rfc.zeromq.org/spec:37/ZMTP/) — ZMTP 3.1 extensions (heartbeating)
- [ZeroMQ Guide](https://zguide.zeromq.org/) — patterns and usage
