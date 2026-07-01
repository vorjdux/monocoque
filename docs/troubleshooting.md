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

4. **HWM reached**: If the sender's send HWM is reached it blocks. Increase HWM
   or add a receiver.

---

## Messages silently dropped

**Symptom**: Sender returns `Ok(())` but receiver never gets the message.

**Causes**:

1. **PUB drops at HWM**: PUB sockets drop messages silently when the subscriber
   queue is full. Use `with_xpub_nodrop(true)` on XPUB to get an error instead,
   or increase `send_hwm`.

2. **ROUTER drops unknown peer**: ROUTER silently drops messages to unknown peers
   by default. Set `with_router_mandatory(true)` to get an error.

3. **Subscription mismatch**: Verify the topic prefix matches the subscription.
   ```
   message topic: "sensor.temperature.room1"
   subscription:  "sensor.temperature"  ← matches
   subscription:  "sensor.temp"         ← does NOT match (prefix must be exact)
   ```

4. **`req_relaxed` with rapid reconnects**: Correlation ID mismatch can cause
   replies to be silently discarded. Enable `with_req_correlate(true)` to get
   explicit errors.

---

## Handshake fails / authentication rejected

**Symptom**: `connect()` or `accept()` returns an authentication error.

1. **CURVE key mismatch**: The client's `curve_serverkey` must exactly match the
   server's `curve_publickey` (32 raw bytes, not Z85-encoded).

2. **PLAIN credentials wrong**: Verify the username/password on both sides match
   exactly (case-sensitive).

3. **No ZAP handler**: If a ZAP domain is set but no handler is running on
   `inproc://zeromq.zap.01`, all PLAIN/CURVE connections are rejected. Start the
   handler before binding the server socket.

4. **Mechanism mismatch**: Both sides must agree on a mechanism. A CURVE client
   cannot connect to a PLAIN server.

---

## `WouldBlock` errors

**Symptom**: `recv()` returns `ErrorKind::WouldBlock` immediately.

This is expected behaviour when `recv_timeout` is set to `Duration::ZERO` (non-blocking
mode) and no message is queued. Either:
- Wait and retry, or
- Remove the timeout: `.with_recv_timeout(Duration::from_secs(5))`.

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

2. **io_uring not active** (default compio backend): On kernels < 5.11, compio
   falls back to thread-pool I/O. Check `uname -r` and update the kernel if
   possible, or build with the tokio backend (`runtime-tokio`) on platforms
   without io_uring.

3. **HWM too low**: If HWM is small, sends block frequently. Increase or set to 0.

4. **Lock contention**: Under many peers, the router/pubsub hub lock may contend.
   Profile with `perf lock record` or `tokio-console`.

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
