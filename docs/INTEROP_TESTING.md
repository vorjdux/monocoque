# Interoperability Testing with libzmq

These examples verify that Monocoque is wire-compatible with libzmq across DEALER/ROUTER, PUB/SUB, and security mechanisms.

## Prerequisites

Install libzmq:

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

For the Python scripts below, install pyzmq: `pip install pyzmq`.

## Running the Examples

Each example runs a Monocoque socket and a libzmq socket in the same process. They exit cleanly on success.

```bash
# Monocoque DEALER <-> libzmq ROUTER
cargo run --example interop_dealer_libzmq --features zmq

# Monocoque ROUTER <-> libzmq DEALER
cargo run --example interop_router_libzmq --features zmq

# Monocoque PUB <-> libzmq SUB
cargo run --example interop_pubsub_libzmq --features zmq
```

Successful output ends with `Interop test completed successfully!`. If the build fails with `error: failed to run custom build command for 'zmq-sys'`, libzmq is not installed or not on the linker path.

## PLAIN Authentication Interop

To test Monocoque against a libzmq PLAIN server, run the ZAP authenticator and server in two terminals, then connect with Monocoque.

Terminal 1 - ZAP authenticator:

```python
from zmq.auth.thread import ThreadAuthenticator
import zmq, time

context = zmq.Context()
auth = ThreadAuthenticator(context)
auth.start()
auth.configure_plain(domain='*', passwords={'alice': 'password123', 'bob': 'secret'})
print("[ZAP] Running. Ctrl+C to stop.")
time.sleep(999999)
```

Terminal 2 - PLAIN server:

```python
import zmq

context = zmq.Context()
socket = context.socket(zmq.REP)
socket.plain_server = True
socket.zap_domain = b'global'
socket.bind("tcp://127.0.0.1:5563")
print("[Server] Listening on port 5563")
while True:
    msg = socket.recv_string()
    socket.send_string(f"Echo: {msg}")
```

Terminal 3 - Monocoque PLAIN client:

```bash
cargo run --example interop_plain_client
```

To test the reverse (Monocoque PLAIN server, libzmq client):

```bash
# Terminal 1
cargo run --example plain_auth_demo

# Terminal 2
python3 <<EOF
import zmq
ctx = zmq.Context()
s = ctx.socket(zmq.REQ)
s.plain_username = b'alice'
s.plain_password = b'password123'
s.connect("tcp://127.0.0.1:5555")
s.send_string("Hello from libzmq")
print("Received:", s.recv_string())
EOF
```

## CURVE Encryption Interop

Monocoque implements CURVE (RFC 26) using X25519 + ChaCha20-Poly1305. Key exchange is compatible with libzmq, but note that libzmq uses Z85 encoding for keys while Monocoque uses raw 32-byte keys - conversion is required.

Generate keypairs:

```python
import zmq.auth
server_public, server_secret = zmq.auth.create_certificates(".", "server")
client_public, client_secret = zmq.auth.create_certificates(".", "client")
```

Run a libzmq CURVE server:

```python
import zmq, zmq.auth

context = zmq.Context()
socket = context.socket(zmq.REP)
pub, sec = zmq.auth.load_certificate("server.key_secret")
socket.curve_secretkey = sec
socket.curve_publickey = pub
socket.curve_server = True
socket.bind("tcp://127.0.0.1:5564")
while True:
    msg = socket.recv_string()
    socket.send_string(f"Encrypted reply: {msg}")
```

Connect with Monocoque, providing the server's public key in hex:

```bash
cargo run --example curve_demo -- --server-key <public_key_hex>
```

## Troubleshooting

**Tests hang and don't exit.** Use the standalone `cargo run --example` commands rather than `cargo test` - the compio runtime has known lifecycle issues inside the test harness.

**Connection refused.** Check that ports 5560–5564 are free before running examples.

## What Is Verified

- ZMTP 3.1 greeting and handshake
- Frame encoding and multipart messages (MORE flag)
- ROUTER identity envelope handling
- PUB/SUB topic filtering
- PLAIN authentication (RFC 23)
- CURVE encryption (RFC 26)
- ZAP protocol (RFC 27)

GSSAPI is not implemented and is not a compatibility target.
