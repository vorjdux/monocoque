# Interoperability Testing with libzmq

Monocoque is tested against real libzmq in two suites, and a few examples let
you watch an exchange by hand.

## Automated suites

**Rust, against the system libzmq** (`monocoque/tests/interop_*.rs`, through the
`zmq` crate): PAIR, PUB/SUB, PUSH/PULL, REQ/REP, ROUTER, STREAM, PLAIN and
CURVE. The CURVE cases skip when the system libzmq was built without CURVE.

```bash
cargo test -p monocoque-rs --features zmq \
    --test interop_pair --test interop_pubsub --test interop_push_pull \
    --test interop_req_rep --test interop_router --test interop_stream \
    --test interop_plain --test interop_curve
```

**Python, against the libzmq bundled with pyzmq** (`interop_tests/`, which has
CURVE built in): REQ/REP, PUB/SUB, and CURVE in both roles, with multipart and
large frames, heartbeats and custom handshake properties. See
[interop_tests/README.md](../interop_tests/README.md).

```bash
pip install pyzmq pytest pytest-timeout
interop_tests/run_all_tests.sh
```

CI runs both in `.github/workflows/interop.yml`.

## Prerequisites

The Rust suite and the examples link the system libzmq:

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

Successful output ends with `interop test completed successfully!` (logged at
INFO level). If the build fails with `error: failed to run custom build command for 'zmq-sys'`, libzmq is not installed or not on the linker path.

## PLAIN Authentication Interop

libzmq checks PLAIN credentials through ZAP, and its ZAP handler has to live
in the server's own process and context. `monocoque/tests/interop_plain.rs`
does exactly that: a libzmq PLAIN server with a ZAP handler on
`inproc://zeromq.zap.01`, and a Monocoque PLAIN client that authenticates and
exchanges a request and reply. Read it as the reference setup.

For Monocoque on both sides, `plain_auth_demo` runs a PLAIN server with a ZAP
handler, a client with valid credentials and one with wrong ones:

```bash
cargo run --example plain_auth_demo --features zmq
```

## CURVE Encryption Interop

Monocoque implements CURVE (RFC 26) with X25519 and XSalsa20-Poly1305 (NaCl
`crypto_box`), and interoperates with libzmq in both roles.
`interop_tests/test_curve_interop.py` is the end-to-end reference: it drives
the `curve_echo` example against pyzmq as both client and server.

libzmq writes keys in Z85; Monocoque takes raw 32-byte keys, which its
examples read as 64 hex characters. pyzmq converts between them:

```python
import binascii, zmq
from zmq.utils import z85
public, secret = zmq.curve_keypair()          # Z85
print(binascii.hexlify(z85.decode(public)))   # hex for Monocoque
```

`curve_demo` shows the Monocoque API on its own:

```bash
cargo run --example curve_demo --features zmq -- keygen            # print a key pair
cargo run --example curve_demo --features zmq -- server <secret_hex>
cargo run --example curve_demo --features zmq -- client <public_hex>
cargo run --example curve_demo --features zmq -- demo              # both in one process
```

## Troubleshooting

**A CURVE test skips.** The Rust CURVE tests need a system libzmq built with
libsodium; the pyzmq suite covers CURVE either way.

**Connection refused.** The examples use fixed loopback ports; check that
nothing else holds them.

**Debugging a handshake.** Set `RUST_LOG=monocoque_zmtp=debug` on the Monocoque
side to log each handshake step.
