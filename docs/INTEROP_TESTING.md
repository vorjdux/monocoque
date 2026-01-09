# Interoperability Testing with libzmq

This directory contains examples that demonstrate Monocoque's compatibility with libzmq.

## Prerequisites

Install libzmq on your system:

```bash
# Ubuntu/Debian
sudo apt install libzmq3-dev

# macOS
brew install zeromq

# Arch Linux
sudo pacman -S zeromq

# Fedora/RHEL
sudo dnf install zeromq-devel
```

## Running Interop Examples

### DEALER ↔ libzmq ROUTER

Tests that Monocoque DEALER can communicate with a libzmq ROUTER:

```bash
cargo run --example interop_dealer_libzmq --features zmq
```

**Expected output:**

```
=== Monocoque ↔ libzmq Interop Test ===

[libzmq ROUTER] Listening on tcp://127.0.0.1:5560
[Monocoque DEALER] Connecting to tcp://127.0.0.1:5560
[Monocoque DEALER] Connected
[libzmq ROUTER] Received from client
  Identity: 5 bytes
  Body: "Ping from Monocoque"
[libzmq ROUTER] Sent reply

[Monocoque DEALER] Sent request

[Monocoque DEALER] Received response:
  Frame 0: "Pong from libzmq"

✅ Interop test completed successfully!
```

### ROUTER ↔ libzmq DEALER

Tests that Monocoque ROUTER can handle libzmq DEALER clients:

```bash
cargo run --example interop_router_libzmq --features zmq
```

**Expected output:**

```
=== Monocoque ROUTER ↔ libzmq DEALER Test ===

[Monocoque ROUTER] Listening on tcp://127.0.0.1:5561
[libzmq DEALER] Connecting to tcp://127.0.0.1:5561
[libzmq DEALER] Connected with identity 'CLIENT_123'
[Monocoque ROUTER] Client connected
[libzmq DEALER] Sent request

[Monocoque ROUTER] Received message:
  Identity (frame 0): 10 bytes
  Body (frame 1): "Request from libzmq"
[Monocoque ROUTER] Sent reply

[libzmq DEALER] Received reply: "Reply from Monocoque ROUTER"

✅ Interop test completed successfully!
```

### PUB ↔ libzmq SUB

Tests that Monocoque PUB can publish to libzmq SUB subscribers:

```bash
cargo run --example interop_pubsub_libzmq --features zmq
```

**Expected output:**

```
=== Monocoque PUB ↔ libzmq SUB Test ===

[Monocoque PUB] Listening on tcp://127.0.0.1:5562
[libzmq SUB] Connected and subscribed to 'topic.*'
[Monocoque PUB] Subscriber connected

[Monocoque PUB] Published: "topic.event.1"
[libzmq SUB] Received message 1: "topic.event.1"
[Monocoque PUB] Published: "topic.event.2"
[libzmq SUB] Received message 2: "topic.event.2"
[Monocoque PUB] Published: "topic.event.3"
[libzmq SUB] Received message 3: "topic.event.3"

✅ PUB/SUB interop test completed successfully!
```

## Troubleshooting

### "error: failed to run custom build command for `zmq-sys`"

The `zmq` crate needs libzmq installed. See prerequisites above.

### Tests hang or don't exit

This is expected behavior - compio runtime lifecycle in test harness has known issues. Use the standalone examples instead (they exit cleanly).

### Connection refused

Make sure no other process is using the ports:

-   5560 (DEALER/ROUTER test)
-   5561 (ROUTER/DEALER test)
-   5562 (PUB/SUB test)

## What These Tests Prove

✅ **ZMTP 3.1 Handshake**: Monocoque correctly implements ZeroMQ greeting and handshake  
✅ **Frame Encoding**: Messages are encoded/decoded compatibly with libzmq  
✅ **Identity Routing**: ROUTER correctly handles identity envelopes  
✅ **Multipart Messages**: Frame MORE flags work correctly  
✅ **Pub/Sub Filtering**: Subscription matching is compatible

## Next Steps

After verifying these examples work:

1. Run full integration tests (when test harness fixed)
2. Add stress tests with multiple peers
3. Test reconnection scenarios
4. Benchmark performance vs libzmq
