# Reliability and Resilience

Monocoque includes three mechanisms to keep sockets well-behaved under load and across network failures: a send high water mark (HWM) to cap buffering, cancellation-safe writes via a poison flag, and automatic reconnection with exponential backoff.

---

## High Water Mark (HWM)

`send_buffered` queues messages in userspace for batched flushing. Without a limit this allocation is unbounded. The HWM enforces a cap: once the buffer reaches `send_hwm` messages, `send_buffered` returns `WouldBlock` instead of queuing more.

```rust
let options = SocketOptions::default().with_send_hwm(100);
let mut dealer = DealerSocket::from_tcp_with_options(stream, options).await?;

for msg in messages {
    match dealer.send_buffered(msg) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            // Buffer full - flush before continuing
            dealer.flush().await?;
        }
        Err(e) => return Err(e),
    }
}
```

The default HWM is 1000 messages. `WouldBlock` is not a fatal error; flush and retry.

A byte-based backpressure system (`SemaphorePermits`) exists in `monocoque-core/src/backpressure.rs` but is not yet wired into the send path. For now, the HWM is message-count only.

---

## Cancellation Safety (Poison Flag)

ZMTP sends multipart messages as sequential frames. If an async `flush()` is cancelled mid-write - for example, by a `timeout()` dropping the future - the peer has received some frames but not all. The stream is now in an invalid state and cannot be recovered.

Monocoque handles this with a poison flag. Before any write, a `PoisonGuard` sets the flag. If the guard is dropped without being explicitly disarmed (i.e., the future is cancelled), the socket is marked poisoned. Subsequent operations on a poisoned socket return `BrokenPipe` immediately.

This applies to all socket types via `SocketBase`.

### The receive path is not poisoned

The poison flag covers writes only. There is no equivalent guard on the read
path, and the two cancel differently.

Cancelling a `recv()` does not lose decoded frames: partial multipart state lives
on the socket, not in the future, so whatever has already been decoded survives
and the next `recv()` continues from it. What can be lost is the single read that
was in flight when the future was dropped, and losing those bytes desyncs the
ZMTP stream. Unlike a cancelled write, this happens silently: the socket is not
marked poisoned and the next call does not return `BrokenPipe`.

In practice this is reachable only when you cancel a `recv()` yourself, by
wrapping it in your own `timeout()` or racing it in a `select!`. Prefer the
built-in `recv_timeout` socket option, which bounds the whole logical receive
internally and leaves the socket usable when it elapses. If you must cancel a
receive externally, treat the socket as suspect afterwards and reconnect rather
than continuing to read from it.

```rust
let result = timeout(Duration::from_secs(5), dealer.flush()).await;

match result {
    Ok(Ok(())) => {}
    Ok(Err(e)) if e.kind() == ErrorKind::BrokenPipe => {
        // Socket poisoned - reconnect
        dealer = DealerSocket::connect("127.0.0.1:5555").await?;
    }
    Err(_timeout) => {
        // Timeout cancelled flush - socket is poisoned
        dealer = DealerSocket::connect("127.0.0.1:5555").await?;
    }
}
```

A poisoned socket cannot be reused. You must create a new connection.

---

## Automatic Reconnection

By default, if the underlying TCP connection drops, the socket becomes permanently unusable. The reconnection API changes this: monocoque stores the endpoint and transparently reconnects on the next send or receive call.

Build the socket with `connect` or `connect_with_options`, which store the
endpoint, then use the reconnecting send and receive calls:

```rust
let mut client = ReqSocket::connect("127.0.0.1:5555").await?;

loop {
    match client.send_with_reconnect(msg.clone()).await {
        Ok(()) => break,
        Err(e) if e.kind() == ErrorKind::NotConnected => {
            // Reconnection attempt is in progress - back off and retry
            sleep(Duration::from_millis(100)).await;
        }
        Err(e) => return Err(e),
    }
}
```

`recv_with_reconnect` works the same way on the receive side, and
`try_reconnect` forces an attempt without sending or receiving.

Reconnection uses exponential backoff from `reconnect_ivl` (100ms by default),
doubling up to `reconnect_ivl_max`, with equal jitter (a delay uniformly in
`[d/2, d]`) so a fleet reconnecting after a shared outage spreads its retries.
The backoff resets on a successful connection.

`from_tcp` and the `from_*` constructors do not store an endpoint, so
reconnection is not available on sockets built from a raw stream.

**Current support** in `monocoque::zmq`: `DealerSocket`, `PullSocket`,
`PushSocket`, `ReqSocket`, and `SubSocket` expose `try_reconnect`, plus the
`send_with_reconnect` / `recv_with_reconnect` pair appropriate to their
direction. `RouterSocket` accepts incoming connections rather than initiating
them, so the model does not apply. A SUB socket replays its stored subscription list to the
new peer after reconnecting, so filters survive the drop.

---

## Inspecting Socket State

`DealerSocket` exposes a few methods for observing internal state:

- `is_connected()` - whether the underlying stream is present
- `is_poisoned()` - whether the socket has been poisoned by a cancelled write
- `buffered_messages()` - number of messages currently queued
- `try_reconnect()` - attempt reconnection manually without sending
