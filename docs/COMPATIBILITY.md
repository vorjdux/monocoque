# Protocol Compatibility

Monocoque implements ZMTP 3.x and is wire-compatible with all ZeroMQ 4.x releases (minimum 4.1). It sends ZMTP 3.0 greetings for backward compatibility and accepts any 3.x peer, so it interoperates with libzmq 4.1 through 4.4 without configuration. Legacy ZMTP 1.0 and 2.0 peers are detected from the greeting and handled through the same downgrade path as libzmq; see [rfc-compliance.md](rfc-compliance.md).

When both peers advertise ZMTP 3.1, Monocoque enables heartbeating via PING/PONG (RFC 37). With a 3.0 peer, heartbeating is disabled automatically.

## Supported socket types

All standard socket types are implemented: DEALER, ROUTER, PUB, SUB, REQ, REP, PUSH, PULL, XPUB, XSUB, PAIR, and STREAM (raw TCP; messages carry the `Peer-Address` metadata). The draft types CLIENT/SERVER, RADIO/DISH, SCATTER/GATHER and CHANNEL are available behind the `draft` feature. Every socket type is multi-peer and can bind and connect several `tcp://`, `ipc://` and `inproc://` endpoints.

## Supported protocol features

- NULL security mechanism (no authentication, default)
- PLAIN security (username/password)
- CURVE security (CurveZMQ public-key encryption, RFC 26), interoperating with libzmq in both roles and carrying custom metadata
- ZAP authentication protocol, with the peer's IP address in the request
- ZMTP framing - 1-byte and 8-byte length frames
- Multipart messages (MORE flag)
- Command frames - READY, PING, PONG
- Heartbeating - ZMTP 3.1 PING/PONG (RFC 37)
- ROUTER fair-queuing and routing IDs
- PUB/SUB topic filtering
- Automatic reconnection with exponential backoff
- Full socket options API equivalent to `zmq_setsockopt`

## Behavioral differences from libzmq

Monocoque is async-first. There is no blocking `send`/`recv` API; all socket operations are `async fn`. This is a design choice, not a limitation - it eliminates the need for thread-per-connection and generally improves performance under load.

Each socket type is its own Rust type, so only the operations valid for it exist (a PUB socket has no `recv`, for example). Pairing is checked during the handshake, as in libzmq: connecting PUB to PUB is refused with an ERROR command.

The wire protocol is byte-for-byte compatible. Framing, handshake, and READY command formats match RFC 23/ZMTP exactly, so Monocoque peers are indistinguishable from libzmq peers on the wire.

## Running interoperability tests

The test suite includes interop tests against a live libzmq installation:

```bash
# Install libzmq
sudo apt install libzmq3-dev   # Ubuntu/Debian
brew install zeromq            # macOS

# Run interop tests
cargo test --package monocoque-rs --features zmq --test interop_pair
cargo test --package monocoque-rs --features zmq --test interop_router
cargo test --package monocoque-rs --features zmq --test interop_pubsub
cargo test --package monocoque-rs --features zmq --test interop_push_pull
```

## References

- [RFC 23/ZMTP](https://rfc.zeromq.org/spec:23/ZMTP/) - ZMTP 3.0 specification
- [RFC 37/ZMTP](https://rfc.zeromq.org/spec:37/ZMTP/) - ZMTP 3.1 extensions (heartbeating)
- [ZeroMQ Guide](https://zguide.zeromq.org/) - patterns and usage
