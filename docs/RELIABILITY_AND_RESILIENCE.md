# Reliability and Resilience

Monocoque includes four mechanisms to keep sockets well-behaved under load and across network failures: high water marks (HWM) to cap buffering, receive and send paths that a timeout or cancellation cannot corrupt, automatic reconnection with a jittered interval and optional exponential backoff, and optional heartbeats that find a peer that vanished without closing.

---

## High Water Mark (HWM)

Every socket keeps a bounded queue per peer in each direction. `send_hwm` caps the messages queued to a peer; `recv_hwm` caps the complete messages waiting to be received from it, and a peer whose receive queue is full stops being read, so a slow consumer backpressures its own peer through the kernel. Both default to 1000 messages; `0` means unbounded, as in libzmq. inproc peers are bounded the same way in each direction.

When the send queue is full, `send` waits for room, bounded by `send_timeout`: with no timeout it waits, with `Duration::ZERO` it fails at once with `Again`, otherwise it fails with `TimedOut` when the timeout elapses. PUB and XPUB drop the message for a subscriber that is at its HWM instead of waiting. ROUTER does not wait either: it drops the message for a peer at its HWM, or fails with `Again` when `router_mandatory` is set.

```rust
let options = SocketOptions::default()
    .with_send_hwm(100)
    .with_send_timeout(Duration::ZERO);
let mut dealer = DealerSocket::with_options(options);
dealer.connect("tcp://127.0.0.1:5555").await?;

for msg in messages {
    match dealer.send(msg).await {
        Ok(()) => {}
        Err(MonocoqueError::Again) => {
            // Queue full: back off, or drop, and try again later
        }
        Err(e) => return Err(e),
    }
}
```

`Again` and `TimedOut` are not fatal; the socket stays usable.

---

## Cancellation Safety

ZMTP sends multipart messages as sequential frames, so a write cut off part way through would leave the stream unrecoverable. Monocoque avoids this by never doing the I/O inside the caller's future. Each wire peer has one persistent reader loop and one persistent writer loop owned by the socket. `send` only queues the whole encoded message on the peer's outbound queue; the writer loop drains that queue. `recv` only takes a complete message from the peer's ready queue; the reader loop decodes into it.

Neither loop is cancelled by a timeout, so no in-flight read or write is dropped. A `send_timeout` or `recv_timeout` that elapses ends only the wait, and the socket stays usable. The same holds when you cancel a `send()` or `recv()` yourself, by wrapping it in your own `timeout()` or racing it in a `select!`: a message is either queued whole or not at all, and no received bytes are lost. Prefer the built-in timeout options anyway; they arm no timer when the operation is ready at once.

```rust
let mut dealer = DealerSocket::with_options(
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
);
dealer.connect("tcp://127.0.0.1:5555").await?;

match dealer.recv().await {
    Ok(msg) => handle(&msg),
    Err(MonocoqueError::TimedOut) => {
        // Nothing arrived in time; the socket is still usable
    }
    Err(e) => return Err(e),
}
```

---

## Automatic Reconnection

Every endpoint passed to `connect` is redialed whenever its connection drops or a dial fails, as in libzmq. There is no separate reconnecting API: sends and receives keep working across the drop, waiting for a peer as they would before the first connection. New sends go to the peers that are up, or wait for one.

By default the redial interval is constant. `reconnect_ivl` is 100 ms and
`reconnect_ivl_max` is zero, which means no backoff, as in libzmq: every retry
waits `reconnect_ivl`. Each wait has equal jitter (a delay uniformly in
`[d/2, d]`), so a fleet reconnecting after a shared outage spreads its retries;
with the defaults that is 50 to 100 ms.

Setting `reconnect_ivl_max` above `reconnect_ivl` turns on exponential backoff:
the interval doubles after each failed dial until it reaches the maximum, with
the same jitter, and resets once a peer comes up.

```rust
let options = SocketOptions::default()
    .with_reconnect_ivl(Duration::from_millis(100))
    .with_reconnect_ivl_max(Duration::from_secs(10)); // 100 ms, 200 ms, ... 10 s
```

`with_max_reconnect_attempts(Some(n))` stops redialing an endpoint after `n`
failed dials in a row; the default `None` retries indefinitely.
`connect_timeout` bounds one dial: a dial that takes longer counts as a failed
attempt. Its default of zero leaves the limit to the OS. A failed handshake
counts as a failed attempt too, and so does a connection whose options (`tos`,
the buffer sizes, keepalive) cannot be applied: the monitor reports
`ConnectFailed` and the endpoint is redialed after the backoff. An accepted
connection whose options cannot be applied is dropped with a warning in the
log.

`disconnect(endpoint)` stops the redialing and drops the connection, including
one whose handshake is still running. It matches the text given to `connect`
first, so an endpoint connected by host name disconnects by the same name even
if the name now resolves to another address.

Bound endpoints accept new connections for as long as the socket lives, so the
model applies to connected endpoints only. A stream handed to a socket with
`attach` is not redialed. A SUB socket replays its stored subscription list to the
new peer after reconnecting, so filters survive the drop.

---

## Heartbeats

A connection whose peer vanished without closing (a pulled cable, a frozen
host) looks idle, not closed. ZMTP heartbeats find it. They are off by default
and turned on with `heartbeat_ivl`:

```rust
let options = SocketOptions::default()
    .with_heartbeat_ivl(Duration::from_secs(5))
    .with_heartbeat_timeout(Duration::from_secs(10));
```

When nothing has been received from a peer for `heartbeat_ivl`, the socket
sends it a PING. If the peer then stays silent for `heartbeat_timeout` (the
interval again when no timeout is set), the connection is closed, and a
connected endpoint is redialed. Any traffic from the peer counts, not only the
PONG. The socket answers a peer's PINGs itself. Peers that speak ZMTP 1.0 or
2.0 have no heartbeat commands and are neither pinged nor timed out.

A PING carries no TTL by default, as in libzmq, so the peer applies only its
own timeouts. `with_heartbeat_ttl` adds one: the peer then closes the
connection when it hears nothing from this socket for that long, which frees
it from a socket that vanished. With a TTL set, a PING is sent every
`heartbeat_ivl` whether or not the connection is idle, because the peer counts
from the last PING. Set the TTL comfortably above the interval, two or three
times it; at or below the interval the peer's timer can run out between two
PINGs. The TTL is sent in steps of 100 ms.

Heartbeats are written at once on a socket with write coalescing, so a batch
that is slow to fill cannot time a connection out.

---

## Inspecting Socket State

Every socket exposes a few methods for observing its state:

- `peer_count()` and `peer_ids()` - the peers currently connected
- `last_endpoint()` - the endpoint most recently bound (`ZMQ_LAST_ENDPOINT`)
- `has_message()` - whether a receive would complete without waiting
- `can_send()` - whether a send would complete without waiting
- `monitor()` - a stream of connection events: bound, connected, accepted, disconnected, a failed connect attempt, a stalled peer. A handshake that fails on the connecting side is reported as `ConnectFailed`; the accepting side reports nothing for it; the reason is logged at warn level by `monocoque_zmtp`.
