# ZeroMQ Compatibility

monocoque targets compatibility with libzmq 4.3.6 and ZMTP 3.1.

## Socket Types

| Socket | Status | Notes |
|--------|--------|-------|
| PAIR | complete | |
| PUB | complete | |
| SUB | complete | topic filtering included |
| REQ | complete | correlation and relaxed modes |
| REP | complete | |
| DEALER | complete | |
| ROUTER | complete | routing_id, connect_routing_id, router_mandatory |
| PUSH | complete | |
| PULL | complete | |
| XPUB | complete | subscription events, verbose/manual modes |
| XSUB | complete | dynamic subscribe/unsubscribe |
| STREAM | not implemented | raw TCP bridging; skipped as niche |

Draft socket types (SERVER, CLIENT, RADIO, DISH, etc.) are not planned until they stabilize in libzmq 5.x.

## Transports

| Transport | Status | Notes |
|-----------|--------|-------|
| tcp:// | complete | IPv4/IPv6, TCP_NODELAY |
| ipc:// | complete | Unix domain sockets, Unix only |
| inproc:// | complete | zero-copy via flume channels |
| pgm:// / epgm:// | not implemented | requires OpenPGM; skipped |
| tipc:// | not implemented | Linux kernel-specific; skipped |

## Security

| Mechanism | Status | Notes |
|-----------|--------|-------|
| NULL | complete | default, no auth |
| PLAIN | complete | username/password, PlainAuthHandler trait |
| CURVE | complete | X25519 + ChaCha20-Poly1305, perfect forward secrecy |
| ZAP | partial | request/response structures exist; full socket integration pending |
| GSSAPI | not implemented | skipped as enterprise niche |

## Socket Options

~45 options implemented out of 60+ in libzmq. All commonly used options are covered.

Implemented: timeouts (rcvtimeo, sndtimeo, linger, handshake, connect), reconnect intervals, high-water marks, routing/identity options, XPUB/XSUB options, TCP keepalive (Linux/macOS/Windows), REQ correlation and relaxed modes, conflate, network tuning (rate, sndbuf, rcvbuf, multicast hops, TOS, MTU), IPv6, bind-to-device, all PLAIN/CURVE/ZAP security options, socket introspection (type, last_endpoint, rcvmore).

Not implemented: ZMQ_EVENTS polling readiness, some advanced STREAM-specific options.

## Proxies

| Feature | Status |
|---------|--------|
| proxy() | complete - bidirectional forwarding with optional capture socket |
| proxy_steerable() | complete - adds PAUSE/RESUME/TERMINATE control socket |
| Legacy devices (QUEUE, FORWARDER, STREAMER) | not implemented - deprecated in libzmq, use proxy() instead |

## Polling

`zmq_poll` / `zmq_poller` are not implemented. Use `futures::select!` or `tokio::select!` instead - Rust async is a better fit for this.

## Protocol

ZMTP 3.1 is fully implemented including multipart messages, socket type negotiation, and identity frames.
