# Reliability and Resilience

Monocoque includes three mechanisms to keep sockets well-behaved under load and across network failures: high water marks (HWM) to cap buffering, receive and send paths that a timeout or cancellation cannot corrupt, and automatic reconnection with exponential backoff.

---

## High Water Mark (HWM)

Every socket keeps a bounded queue per peer in each direction. `send_hwm` caps the messages queued to a peer; `recv_hwm` caps the complete messages waiting to be received from it, and a peer whose receive queue is full stops being read, so a slow consumer backpressures its own peer through the kernel. Both default to 1000 messages; `0` means unbounded, as in libzmq. inproc peers are bounded the same way in each direction.

When the send queue is full, `send` waits for room, bounded by `send_timeout`: with no timeout it waits, with `Duration::ZERO` it fails at once with `Again`, otherwise it fails with `TimedOut` when the timeout elapses. PUB and XPUB drop the message for a subscriber that is at its HWM instead of waiting (unless `with_xpub_nodrop(true)`).

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

Reconnection uses exponential backoff from `reconnect_ivl` (100ms by default),
doubling up to `reconnect_ivl_max`, with equal jitter (a delay uniformly in
`[d/2, d]`) so a fleet reconnecting after a shared outage spreads its retries.
The backoff resets on a successful connection. `with_max_reconnect_attempts(Some(n))`
stops redialing an endpoint after `n` failed dials in a row; the default `None`
retries indefinitely.

Bound endpoints accept new connections for as long as the socket lives, so the
model applies to connected endpoints only. A stream handed to a socket with
`attach` is not redialed. A SUB socket replays its stored subscription list to the
new peer after reconnecting, so filters survive the drop.

---

## Inspecting Socket State

Every socket exposes a few methods for observing its state:

- `peer_count()` and `peer_ids()` - the peers currently connected
- `last_endpoint()` - the endpoint most recently bound (`ZMQ_LAST_ENDPOINT`)
- `has_message()` - whether a receive would complete without waiting
- `can_send()` - whether a send would complete without waiting
- `monitor()` - a stream of connection events (connected, disconnected, handshake failures, and so on)
