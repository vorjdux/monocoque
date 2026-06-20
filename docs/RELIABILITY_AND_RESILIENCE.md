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

```rust
let result = timeout(Duration::from_secs(5), dealer.flush()).await;

match result {
    Ok(Ok(())) => {}
    Ok(Err(e)) if e.kind() == ErrorKind::BrokenPipe => {
        // Socket poisoned - reconnect
        dealer = DealerSocket::connect_with_reconnect("tcp://127.0.0.1:5555").await?;
    }
    Err(_timeout) => {
        // Timeout cancelled flush - socket is poisoned
        dealer = DealerSocket::connect_with_reconnect("tcp://127.0.0.1:5555").await?;
    }
}
```

A poisoned socket cannot be reused. You must create a new connection.

---

## Automatic Reconnection

By default, if the underlying TCP connection drops, the socket becomes permanently unusable. The reconnection API changes this: monocoque stores the endpoint and transparently reconnects on the next send or receive call.

Use `connect_with_reconnect` instead of building a socket from a raw stream:

```rust
let mut dealer = DealerSocket::connect_with_reconnect("tcp://127.0.0.1:5555").await?;

loop {
    match dealer.send_with_reconnect(msg.clone()).await {
        Ok(()) => break,
        Err(e) if e.kind() == ErrorKind::NotConnected => {
            // Reconnection attempt is in progress - back off and retry
            sleep(Duration::from_millis(100)).await;
        }
        Err(e) => return Err(e),
    }
}
```

`recv_with_reconnect` works the same way on the receive side.

Reconnection uses exponential backoff: 100ms initially, doubling on each failure up to 30 seconds, with ±25% jitter to avoid thundering herd. The backoff resets on a successful connection.

The original `from_tcp` and `connect` APIs still work as before. They do not store an endpoint, so reconnection is not available on those sockets.

**Current support**: `DealerSocket` only. SUB sockets need to re-subscribe on reconnect (not yet implemented). REQ sockets have a request/reply state machine that complicates mid-flight reconnection. ROUTER sockets accept incoming connections rather than initiating them, so this model does not apply directly.

---

## Inspecting Socket State

`DealerSocket` exposes a few methods for observing internal state:

- `is_connected()` - whether the underlying stream is present
- `is_poisoned()` - whether the socket has been poisoned by a cancelled write
- `buffered_messages()` - number of messages currently queued
- `try_reconnect()` - attempt reconnection manually without sending
