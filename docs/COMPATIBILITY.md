# Protocol Compatibility

Monocoque implements ZMTP 3. It announces version 3.0 in its greeting and accepts any 3.x peer, and it supports the ZMTP 3.1 additions: PING/PONG heartbeats, SUBSCRIBE and CANCEL commands on receive, and the draft socket types. That makes it wire-compatible with libzmq 4.1 and later without configuration. The interop tests run against libzmq 4.3.4 (built from source by the `zmq` crate) and 4.3.5 (bundled with pyzmq); other versions are expected to work but are not tested. Legacy ZMTP 1.0 and 2.0 peers are detected from the greeting and handled through the same downgrade path as libzmq; see [rfc-compliance.md](rfc-compliance.md).

Heartbeating via PING/PONG (RFC 37) is off by default. Setting `heartbeat_ivl` turns it on: the socket then sends a PING on a connection it has heard nothing from for that long, to ZMTP 3.0 and 3.1 peers alike, and closes the connection when the peer stays silent past `heartbeat_timeout`. A PING carries no TTL unless `heartbeat_ttl` is set, as in libzmq. With a TTL the peer closes the connection when it hears nothing for that long, so a PING is then sent every `heartbeat_ivl` whether or not the connection is idle; set the TTL comfortably above the interval. ZMTP 1.0 and 2.0 have no heartbeat commands, so legacy peers are never sent one.

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
- Heartbeating - ZMTP 3.1 PING/PONG (RFC 37), opt-in with `heartbeat_ivl`
- ROUTER fair-queuing and routing IDs
- PUB/SUB topic filtering
- Automatic reconnection, at a constant, jittered interval by default and with exponential backoff when `reconnect_ivl_max` is set
- Socket options through `SocketOptions`, covering most of what `zmq_setsockopt` offers. Some libzmq options are not implemented; [MIGRATION.md](MIGRATION.md) lists them

## Behavioral differences from libzmq

Monocoque is async-first. There is no blocking `send`/`recv` API; all socket operations are `async fn`. This is a design choice, not a limitation - it eliminates the need for thread-per-connection and generally improves performance under load.

Each socket type is its own Rust type, so only the operations valid for it exist (a PUB socket has no `recv`, for example). Pairing is checked during the handshake, as in libzmq: connecting PUB to PUB is refused and the connection is closed. monocoque sends an ERROR command first; libzmq 4.3.5 closes without one.

The wire protocol is byte-for-byte compatible. Framing, handshake, and READY command formats match RFC 23/ZMTP exactly, so Monocoque peers are indistinguishable from libzmq peers on the wire.

## Running interoperability tests

The test suite includes interop tests against real libzmq, one file per area in `monocoque/tests/interop_*.rs`:

```bash
cargo test -p monocoque-rs --features zmq \
    --test interop_pair --test interop_pubsub --test interop_push_pull \
    --test interop_req_rep --test interop_router --test interop_stream \
    --test interop_plain --test interop_curve --test interop_heartbeat
```

[INTEROP_TESTING.md](INTEROP_TESTING.md) describes them, the pyzmq suite beside them, and what each needs installed.

## References

- [RFC 23/ZMTP](https://rfc.zeromq.org/spec:23/ZMTP/) - ZMTP 3.0 specification
- [RFC 37/ZMTP](https://rfc.zeromq.org/spec:37/ZMTP/) - ZMTP 3.1 extensions (heartbeating)
- [ZeroMQ Guide](https://zguide.zeromq.org/) - patterns and usage
