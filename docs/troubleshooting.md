# Troubleshooting

---

## Connection hangs / never completes

**Symptom**: `connect()` or `bind()` returns successfully but `recv()` blocks forever.

**Causes and fixes**:

1. **ZMTP handshake timeout**: The remote peer is not speaking ZMTP (e.g. it's a
   raw TCP server). Check that both sides use compatible socket types:
   - PUB ↔ SUB / XSUB
   - PUSH ↔ PULL
   - REQ ↔ REP
   - DEALER ↔ ROUTER

2. **No subscription**: A SUB socket with no subscriptions receives nothing.
   ```rust
   sub.subscribe(b"").await?; // subscribe to all topics
   ```

3. **Wrong port / firewall**: Verify the port with `ss -tlnp | grep <port>`.

4. **HWM reached**: If the sender's send HWM is reached it waits (or fails with
   `Again`/`TimedOut` under a send timeout). Increase HWM or add a receiver.

---

## Messages silently dropped

**Symptom**: Sender returns `Ok(())` but receiver never gets the message.

**Causes**:

1. **PUB drops at HWM**: PUB sockets drop messages silently when the subscriber
   queue is full. Increase `send_hwm`, or turn on write coalescing so each
   subscriber's queue drains in larger writes.

2. **ROUTER drops unknown peer**: ROUTER silently drops messages to unknown peers
   by default. Set `with_router_mandatory(true)` to get an error.

3. **Subscription mismatch**: Verify the topic prefix matches the subscription.
   ```
   message topic: "sensor.temperature.room1"
   subscription:  "sensor.temperature"  ← matches
   subscription:  "sensor.temp"         ← does NOT match (prefix must be exact)
   ```

4. **`req_relaxed` without `req_correlate`**: After a relaxed REQ abandons a
   request, a late reply to it can be returned as the reply to the next one.
   Set `with_req_correlate(true)` as well: each request then carries an id,
   and a reply whose id does not match the outstanding request is skipped.
   The skip is silent. No error is returned, and `recv()` goes on waiting for
   the matching reply, or until `recv_timeout`.

---

## Handshake fails / authentication rejected

**Symptom**: Sends or receives on the client time out. A rejected peer never
becomes a connection, so `connect()` itself still returns `Ok`. On the
connecting side the socket monitor reports each failed handshake as
`SocketEvent::ConnectFailed`, with the reason as text, and the endpoint is
redialed after the reconnect interval. There is no separate handshake event.
The accepting side gets no monitor event for a handshake that fails; it only
logs the failure at debug level (`RUST_LOG=monocoque_core=debug`, with a
tracing subscriber installed).

1. **CURVE key mismatch**: The client's `curve_serverkey` must exactly match the
   public key the server passes to `with_curve_keypair` (32 raw bytes, not
   Z85-encoded).

2. **PLAIN credentials wrong**: Verify the username/password on both sides match
   exactly (case-sensitive).

3. **No ZAP handler**: A PLAIN server refuses every client when no handler is
   running on `inproc://zeromq.zap.01`, whether or not a ZAP domain is set. A
   CURVE server with a ZAP domain does the same; without a domain it admits
   any client key. Start the handler before binding the server socket.

4. **Mechanism mismatch**: Both sides must agree on a mechanism. A CURVE client
   cannot connect to a PLAIN server.

5. **Backoff after repeated failures**: After two failed authentications in a
   row from one IP, the server delays each new handshake from that address
   (100 ms doubling up to 10 s). Fix the credentials and the delay clears on the
   next success. See "Repeated failures" in [SECURITY_GUIDE.md](SECURITY_GUIDE.md).

---

## `Again` errors

**Symptom**: `recv()` returns `MonocoqueError::Again` immediately.

This is expected behaviour when `recv_timeout` is set to `Duration::ZERO` (non-blocking
mode) and no message is queued. Either:
- Wait and retry, or
- Use a non-zero timeout: `.with_recv_timeout(Duration::from_secs(5))`, in which
  case an empty wait ends in `MonocoqueError::TimedOut`.

---

## Reconnection not happening

**Symptom**: After a peer disconnects, messages fail permanently instead of reconnecting.

1. **`max_reconnect_attempts` exhausted**: Set to `None` for unlimited retries.
   ```rust
   .with_max_reconnect_attempts(None)
   ```

2. **Send timeout too short**: If `send_timeout` fires before the reconnection
   succeeds, the operation fails rather than waiting for reconnect.

3. **Inproc transport**: Inproc endpoints do not reconnect. The server must be
   bound before the client connects.

---

## Performance regressions

**Symptom**: Throughput drops or latency spikes unexpectedly.

1. **Buffer too small**: Read buffer smaller than your message causes fragmented
   reads. Increase with `.with_read_buffer_size(n)`.

2. **io_uring not available** (default compio backend): The default backend
   is built for io_uring on Linux. On an old kernel (check `uname -r`), or in a
   container or sandbox that blocks io_uring, update the kernel or lift the
   restriction if possible, or build with a portable backend: the tokio backend
   (`runtime-tokio`) and the smol backend (`runtime-smol`) are equally valid
   choices.

3. **HWM too low**: If HWM is small, sends block frequently. Increase or set to 0.

4. **One socket per core**: Each socket runs all its peers on the runtime thread
   that owns it. If one socket saturates a core, split the load across several
   sockets on separate threads.

---

## Runtime panics

**Symptom**: A task panics with "BUG:" prefix.

These are invariant violations in monocoque's internal state. Please file a bug
report at https://github.com/vorjdux/monocoque/issues with:
- The full panic message (including "BUG:" text)
- The socket type and configuration
- A minimal reproducer if possible

---

## Fuzz crashes

If a fuzz target in `monocoque-fuzz/` produces a crash artifact:

```bash
# Reproduce the crash
cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_frame_codec -- \
    monocoque-fuzz/artifacts/fuzz_frame_codec/<crash-file>
```

Then minimise:
```bash
cargo +nightly fuzz tmin --fuzz-dir monocoque-fuzz fuzz_frame_codec -- \
    monocoque-fuzz/artifacts/fuzz_frame_codec/<crash-file>
```

File a security bug report if the crash is in a parsing or decoding path.
