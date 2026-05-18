# Migrating from libzmq

This guide maps libzmq C API concepts to their monocoque equivalents.

> **API layers**: monocoque exposes two levels. The high-level `monocoque::zmq` module
> (feature `zmq`) provides ergonomic `bind`/`connect` constructors and uniform `recv`/`send`.
> The lower-level `monocoque_zmtp` crate exposes individual socket structs that require
> manual `TcpListener` management. Examples below use the high-level API.

---

## Socket creation

| libzmq | monocoque |
|--------|-----------|
| `zmq_ctx_new()` + `zmq_socket(ctx, ZMQ_PUSH)` | `PushSocket::connect("tcp://…").await?` |
| `zmq_bind(sock, "tcp://*:5555")` | `PushSocket::bind("tcp://0.0.0.0:5555").await?` |
| `zmq_connect(sock, "tcp://…")` | `DealerSocket::connect("tcp://…").await?` |
| `zmq_close(sock)` | Drop the socket — Rust RAII handles cleanup |
| `zmq_ctx_destroy(ctx)` | Not needed — no global context |

---

## Socket types

| libzmq type | monocoque type | Notes |
|-------------|---------------|-------|
| `ZMQ_REQ` | `ReqSocket` | |
| `ZMQ_REP` | `RepSocket` | |
| `ZMQ_DEALER` | `DealerSocket` | |
| `ZMQ_ROUTER` | `RouterSocket` | |
| `ZMQ_PUB` | `PubSocket` | |
| `ZMQ_SUB` | `SubSocket` | |
| `ZMQ_XPUB` | `XPubSocket` | |
| `ZMQ_XSUB` | `XSubSocket` | |
| `ZMQ_PUSH` | `PushSocket` | |
| `ZMQ_PULL` | `PullSocket` | |
| `ZMQ_PAIR` | `PairSocket` | |
| `ZMQ_STREAM` | `StreamSocket` | Raw TCP bridging |
| `ZMQ_SERVER` / `ZMQ_CLIENT` | Not implemented | Draft API |
| `ZMQ_RADIO` / `ZMQ_DISH` | Not implemented | Draft API |

---

## Socket options (`zmq_setsockopt`)

| libzmq constant | monocoque `SocketOptions` method |
|-----------------|----------------------------------|
| `ZMQ_RCVTIMEO` | `.with_recv_timeout(Duration::…)` |
| `ZMQ_SNDTIMEO` | `.with_send_timeout(Duration::…)` |
| `ZMQ_LINGER` | `.with_linger(Some(Duration::…))` |
| `ZMQ_RCVHWM` | `.with_recv_hwm(n)` |
| `ZMQ_SNDHWM` | `.with_send_hwm(n)` |
| `ZMQ_IDENTITY` / `ZMQ_ROUTING_ID` | `.with_routing_id(Bytes::…)` |
| `ZMQ_ROUTER_MANDATORY` | `.with_router_mandatory(true)` |
| `ZMQ_ROUTER_HANDOVER` | `.with_router_handover(true)` |
| `ZMQ_PROBE_ROUTER` | `.with_probe_router(true)` |
| `ZMQ_SUBSCRIBE` | `.with_subscribe(prefix)` |
| `ZMQ_UNSUBSCRIBE` | `.with_unsubscribe(prefix)` |
| `ZMQ_XPUB_VERBOSE` | `.with_xpub_verbose(true)` |
| `ZMQ_XPUB_MANUAL` | `.with_xpub_manual(true)` |
| `ZMQ_XPUB_WELCOME_MSG` | `.with_xpub_welcome_msg(bytes)` |
| `ZMQ_XPUB_NODROP` | `.with_xpub_nodrop(true)` |
| `ZMQ_INVERT_MATCHING` | `.with_invert_matching(true)` |
| `ZMQ_IMMEDIATE` | `.with_immediate(true)` |
| `ZMQ_CONFLATE` | `.with_conflate(true)` |
| `ZMQ_MAXMSGSIZE` | `.with_max_msg_size(Some(n))` |
| `ZMQ_RECONNECT_IVL` | `.with_reconnect_ivl(Duration::…)` |
| `ZMQ_RECONNECT_IVL_MAX` | `.with_reconnect_ivl_max(Duration::…)` |
| `ZMQ_CONNECT_TIMEOUT` | `.with_connect_timeout(Duration::…)` |
| `ZMQ_HANDSHAKE_IVL` | `.with_handshake_timeout(Duration::…)` |
| `ZMQ_HEARTBEAT_IVL` | `.with_heartbeat_ivl(Duration::…)` |
| `ZMQ_HEARTBEAT_TTL` | `.with_heartbeat_ttl(Duration::…)` |
| `ZMQ_HEARTBEAT_TIMEOUT` | `.with_heartbeat_timeout(Duration::…)` |
| `ZMQ_TCP_KEEPALIVE` | `.tcp_keepalive` field |
| `ZMQ_IPV6` | `.ipv6` field |
| `ZMQ_PLAIN_SERVER` | `.with_plain_server(true)` |
| `ZMQ_PLAIN_USERNAME` + `ZMQ_PLAIN_PASSWORD` | `.with_plain_credentials("user", "pass")` |
| `ZMQ_CURVE_SERVER` | `.with_curve_server(true)` |
| `ZMQ_CURVE_PUBLICKEY` + `ZMQ_CURVE_SECRETKEY` | `.with_curve_keypair(pub_bytes, sec_bytes)` |
| `ZMQ_CURVE_SERVERKEY` | `.with_curve_serverkey(server_pub_bytes)` |
| `ZMQ_ZAP_DOMAIN` | `.with_zap_domain("domain")` |
| `ZMQ_ROUTER_RAW` | `.with_router_raw(true)` |
| `ZMQ_STREAM_NOTIFY` | `.with_stream_notify(true/false)` |

> **Note on PLAIN/CURVE options**: Unlike libzmq which sets username and password
> as separate `zmq_setsockopt` calls, monocoque combines them: `.with_plain_credentials(u, p)`
> and `.with_curve_keypair(pub, sec)`. This prevents partial/invalid configuration.

---

## Send / receive

| libzmq | monocoque |
|--------|-----------|
| `zmq_send(sock, data, len, 0)` | `socket.send(vec![Bytes::from(data)]).await?` |
| `zmq_send(sock, part, len, ZMQ_SNDMORE)` | Include multiple frames in the Vec |
| `zmq_recv(sock, buf, len, 0)` | `let frames = socket.recv().await` |
| `zmq_msg_send` / `zmq_msg_recv` | Same as above — monocoque always uses `Vec<Bytes>` |

> **`recv()` return type**: High-level sockets return `Option<Vec<Bytes>>` (None on disconnect)
> or `io::Result<Option<Vec<Bytes>>>`. Use pattern matching rather than `?` directly.

---

## Polling (`zmq_poll`)

libzmq's `zmq_poll` multiplexes multiple sockets synchronously. In monocoque,
use Rust async primitives instead:

```rust
// libzmq style (blocking)
// zmq_poll(items, 2, timeout_ms);

// monocoque style — race two receives
use futures::future;

let result = future::select(
    Box::pin(socket_a.recv()),
    Box::pin(socket_b.recv()),
).await;

match result {
    future::Either::Left((msg, _)) => { /* socket_a got a message */ }
    future::Either::Right((msg, _)) => { /* socket_b got a message */ }
}
```

For more than two sockets, use `futures::select!` or spawn each socket in
its own task communicating over a `flume` channel.

---

## Transports

| libzmq URI | monocoque URI |
|------------|--------------|
| `tcp://host:port` | `tcp://host:port` |
| `inproc://name` | `inproc://name` |
| `ipc:///path/to/socket` | `ipc:///path/to/socket` (Linux/macOS) |
| `pgm://…` / `epgm://…` | Not implemented |
| `tipc://…` | Not implemented |
| `vmci://…` | Not implemented |

---

## Context / threading

libzmq uses a context object and internal I/O threads. monocoque uses compio's
per-thread runtime — there is no global context.

- **No `zmq_ctx_set(ZMQ_IO_THREADS, n)`**: Use one compio runtime per OS thread.
- **No `zmq_ctx_set(ZMQ_MAX_SOCKETS, n)`**: No global socket limit.
- **Thread safety**: monocoque sockets are `!Send`; don't move them across threads.
  Use `inproc` channels for cross-thread communication.

---

## Error codes

| libzmq errno | monocoque equivalent |
|--------------|----------------------|
| `EAGAIN` | `io::ErrorKind::WouldBlock` |
| `EFSM` | `io::ErrorKind::Other` ("socket not in correct state") |
| `ETERM` | Socket/runtime dropped |
| `ENOTSUP` | `io::ErrorKind::Unsupported` |
| `EHOSTUNREACH` | `io::ErrorKind::Other` ("peer not reachable") |

---

## Common patterns

### Request-Reply

These examples use the high-level `monocoque::zmq` API (feature `zmq`).

```rust
use monocoque::zmq::{ReqSocket, RepSocket};
use bytes::Bytes;

// REQ side
let mut req = ReqSocket::connect("tcp://127.0.0.1:5555").await?;
req.send(vec![Bytes::from("Hello")]).await?;
let reply = req.recv().await; // returns Option<Vec<Bytes>>

// REP side (in another task/thread)
let mut rep = RepSocket::bind("tcp://127.0.0.1:5555").await?;
let request = rep.recv().await; // returns Option<Vec<Bytes>>
if let Some(msg) = request {
    rep.send(msg).await?; // echo
}
```

### Pub-Sub

```rust
use monocoque::zmq::{PubSocket, SubSocket};
use bytes::Bytes;

// PUB
let mut pub_sock = PubSocket::bind("tcp://0.0.0.0:5556").await?;
pub_sock.send(vec![Bytes::from("topic data")]).await?;

// SUB
let mut sub = SubSocket::connect("tcp://127.0.0.1:5556").await?;
sub.subscribe(b"topic").await?;
let msg = sub.recv().await; // returns io::Result<Option<Vec<Bytes>>>
```
