# Identity Routing in ROUTER Sockets

ROUTER sockets deliver messages to specific peers rather than load-balancing or broadcasting. To do that, every connected peer must have an identity - a byte string that the ROUTER uses as an address.

By default a connecting socket announces no identity, and the ROUTER gives the connection a generated one: five bytes, a zero byte and a counter, new on every reconnect. You can override this with a custom identity set on the connecting socket, or, when the ROUTER itself connects out, by naming the peer at the other end with `with_connect_routing_id` before calling `connect` (`ZMQ_CONNECT_ROUTING_ID`).

---

## Setting a Custom Identity

A DEALER (or REQ) socket announces its identity during the ZMTP handshake. Set it via `SocketOptions`:

```rust
let options = SocketOptions::default().with_routing_id(Bytes::from_static(b"worker-001"));
let mut dealer = DealerSocket::with_options(options);
dealer.connect("tcp://127.0.0.1:5555").await?;
```

The ROUTER then sees `"worker-001"` as the routing key for that peer.

Identity constraints:
- 1 to 255 bytes, as in libzmq. `with_routing_id` stores the value as given, and `bind`, `connect` and `attach` fail with `InvalidRoutingId` for an empty or longer one
- Should not start with a null byte (`0x00`), which is reserved for auto-generated identities. It is accepted, as libzmq accepts it, but a ROUTER replaces an identity that starts with `0x00` with a generated one

---

## Sending to a Specific Peer

When sending from a ROUTER, the first frame is the peer's identity. The ROUTER removes it and sends the remaining frames to that peer unchanged. What those frames must be depends on the peer's socket type.

To a DEALER, send the identity and then the payload:

```rust
router.send(Msg::from([
    Bytes::from("worker-001"),
    Bytes::from("task"),
])).await?;
```

To a REQ, put an empty delimiter frame between them. A REQ expects it in front of every reply and drops a reply without one; a DEALER would receive it as an empty first frame of the message.

```rust
router.send(Msg::from([
    Bytes::from("client-001"),
    Bytes::new(),            // delimiter, for a REQ peer only
    Bytes::from("reply"),
])).await?;
```

When receiving on a ROUTER, the sender's identity is prepended as the first frame. The rest is what the peer put on the wire: `[identity, payload]` from a DEALER, `[identity, "", payload]` from a REQ, which adds the delimiter itself. Replying with the frames as received, payload replaced, is therefore right for either.

---

## ROUTER_MANDATORY

By default, a ROUTER silently drops messages addressed to an unknown identity. Enable `router_mandatory` to get an error instead:

```rust
let mut router = RouterSocket::with_options(
    SocketOptions::default().with_router_mandatory(true),
);

// Now returns Err(MonocoqueError::HostUnreachable) if identity is unknown
router.send(Msg::from([Bytes::from("missing-peer"), Bytes::from("msg")])).await?;
```

Under `router_mandatory`, a send to a peer at its HWM fails with `Again` instead of being dropped. This is useful during development or in systems where a missing peer is a bug rather than a normal condition.

---

## ROUTER_HANDOVER

If a second client connects with an identity already held by an existing connection, the ROUTER refuses it by default: the new connection is closed and the old one keeps the identity. Enable `router_handover` to let the new connection take over that identity instead:

```rust
let mut router = RouterSocket::with_options(
    SocketOptions::default().with_router_handover(true),
);
```

From then on a send to that identity goes to the new connection. The old connection is not closed by the handover, which is where monocoque differs from libzmq: messages it still sends are received under the same identity, messages already queued for it are still written to it, and nothing new can be addressed to it. It goes away when its peer closes it or a heartbeat times it out.

This is useful for reconnecting clients that want to reclaim a stable identity without the ROUTER needing to notice the old connection dropped first.

The option is read when the socket is created; changing it on a live socket has no effect.

---

## ZeroMQ Option Correspondence

| monocoque API | ZeroMQ option |
|---|---|
| `SocketOptions::with_routing_id` | `ZMQ_ROUTING_ID` (5) |
| `SocketOptions::with_connect_routing_id` | `ZMQ_CONNECT_ROUTING_ID` (61) |
| `SocketOptions::with_router_mandatory` | `ZMQ_ROUTER_MANDATORY` (33) |
| `SocketOptions::with_router_handover` | `ZMQ_ROUTER_HANDOVER` (56) |
