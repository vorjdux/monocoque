# Identity Routing in ROUTER Sockets

ROUTER sockets deliver messages to specific peers rather than load-balancing or broadcasting. To do that, every connected peer must have an identity - a byte string that the ROUTER uses as an address.

By default, monocoque assigns a random identity when a peer connects. You can override this with a custom identity set on the connecting socket, or, when the ROUTER itself connects out, by naming the peer at the other end with `with_connect_routing_id` before calling `connect` (`ZMQ_CONNECT_ROUTING_ID`).

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
- 1 to 255 bytes
- Cannot start with a null byte (`0x00`), which is reserved for auto-generated identities

---

## Sending to a Specific Peer

When sending from a ROUTER, the first frame is the peer's identity, followed by an empty delimiter frame, then the message payload:

```rust
router.send(Msg::from([
    Bytes::from("worker-001"),
    Bytes::new(),            // delimiter
    Bytes::from("task"),
])).await?;
```

When receiving on a ROUTER, the incoming message is prepended with the sender's identity in the same way.

---

## ROUTER_MANDATORY

By default, a ROUTER silently drops messages addressed to an unknown identity. Enable `router_mandatory` to get an error instead:

```rust
let mut router = RouterSocket::with_options(
    SocketOptions::default().with_router_mandatory(true),
);

// Now returns Err(MonocoqueError::HostUnreachable) if identity is unknown
router.send(Msg::from([Bytes::from("missing-peer"), Bytes::new(), Bytes::from("msg")])).await?;
```

Under `router_mandatory`, a send to a peer at its HWM fails with `Again` instead of being dropped. This is useful during development or in systems where a missing peer is a bug rather than a normal condition.

---

## ROUTER_HANDOVER

If a second client connects with an identity already held by an existing connection, the ROUTER rejects it by default. Enable `router_handover` to let the new connection take over that identity instead, closing the old one:

```rust
let mut router = RouterSocket::with_options(
    SocketOptions::default().with_router_handover(true),
);
```

This is useful for reconnecting clients that want to reclaim a stable identity without the ROUTER needing to notice the old connection dropped first.

---

## ZeroMQ Option Correspondence

| monocoque API | ZeroMQ option |
|---|---|
| `SocketOptions::with_routing_id` | `ZMQ_ROUTING_ID` (5) |
| `SocketOptions::with_connect_routing_id` | `ZMQ_CONNECT_ROUTING_ID` (61) |
| `SocketOptions::with_router_mandatory` | `ZMQ_ROUTER_MANDATORY` (33) |
| `SocketOptions::with_router_handover` | `ZMQ_ROUTER_HANDOVER` (56) |
