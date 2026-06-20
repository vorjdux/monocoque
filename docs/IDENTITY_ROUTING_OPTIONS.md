# Identity Routing in ROUTER Sockets

ROUTER sockets deliver messages to specific peers rather than load-balancing or broadcasting. To do that, every connected peer must have an identity — a byte string that the ROUTER uses as an address.

By default, monocoque assigns a random identity when a peer connects. You can override this with a custom identity set on the connecting socket, or by assigning one on the ROUTER side before the connection is accepted.

---

## Setting a Custom Identity

A DEALER (or REQ) socket announces its identity during the ZMTP handshake. Set it via `SocketOptions`:

```rust
let options = SocketOptions::default().with_identity(b"worker-001".to_vec());
let mut dealer = DealerSocket::connect_with_options("tcp://127.0.0.1:5555", options).await?;
```

The ROUTER then sees `"worker-001"` as the routing key for that peer.

Identity constraints:
- 1 to 255 bytes
- Cannot start with a null byte (`0x00`), which is reserved for auto-generated identities

---

## Sending to a Specific Peer

When sending from a ROUTER, the first frame is the peer's identity, followed by an empty delimiter frame, then the message payload:

```rust
router.send(vec![
    Bytes::from("worker-001"),
    Bytes::new(),            // delimiter
    Bytes::from("task"),
]).await?;
```

When receiving on a ROUTER, the incoming message is prepended with the sender's identity in the same way.

---

## ROUTER_MANDATORY

By default, a ROUTER silently drops messages addressed to an unknown identity. Enable `router_mandatory` to get an error instead:

```rust
router.set_router_mandatory(true);

// Now returns Err(HostUnreachable) if identity is unknown
router.send(vec![Bytes::from("missing-peer"), Bytes::new(), Bytes::from("msg")]).await?;
```

This is useful during development or in systems where a missing peer is a bug rather than a normal condition.

---

## ROUTER_HANDOVER

If a second client connects with an identity already held by an existing connection, the ROUTER rejects it by default. Enable `router_handover` to let the new connection take over that identity instead, closing the old one:

```rust
router.set_router_handover(true);
```

This is useful for reconnecting clients that want to reclaim a stable identity without the ROUTER needing to notice the old connection dropped first.

---

## ZeroMQ Option Correspondence

| monocoque API | ZeroMQ option |
|---|---|
| `SocketOptions::with_identity` | `ZMQ_ROUTING_ID` (61) |
| `RouterSocket::set_router_mandatory` | `ZMQ_ROUTER_MANDATORY` (33) |
| `RouterSocket::set_router_handover` | `ZMQ_ROUTER_HANDOVER` (56) |
