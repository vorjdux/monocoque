# libzmq interoperability tests (pyzmq)

These tests run Monocoque against the libzmq that ships inside pyzmq's wheels,
which is built with CURVE. They complement the Rust tests in
`monocoque/tests/interop_*.rs`, which link the system libzmq; those skip their
CURVE cases when the system library lacks it, so CURVE interop is covered here.

## What is covered

| File | Cases |
|---|---|
| `test_req_rep_interop.py` | libzmq REQ to Monocoque REP and the reverse, multipart, repeated cycles, a 1 MB message |
| `test_pub_sub_interop.py` | libzmq PUB to Monocoque SUB and the reverse, topic filtering |
| `test_curve_interop.py` | CURVE both ways (client and server roles), multipart with an empty frame and a 70 KB frame, heartbeats inside the encrypted stream, custom handshake properties both ways |

The Monocoque side of each test is one of the examples `simple_rep_server`,
`simple_req_client` and `curve_echo`, run from `target/debug/examples`.

## Running

```bash
pip install pyzmq pytest pytest-timeout
interop_tests/run_all_tests.sh
```

The script builds the three examples and runs the suite. To run one file by
hand, build the examples first:

```bash
cargo build -p monocoque-rs --features zmq \
    --example simple_rep_server --example simple_req_client --example curve_echo
cd interop_tests && python3 -m pytest -v test_curve_interop.py
```

CI runs the same suite in `.github/workflows/interop.yml`.

## Troubleshooting

- **A test skips CURVE:** `python3 -c "import zmq; print(zmq.has('curve'))"`
  must print `True`. pyzmq's wheels have it; a pyzmq built against a system
  libzmq without libsodium does not.
- **Binary not found:** the tests run the debug builds; build them as above.
- **Debug a handshake:** `RUST_LOG=monocoque_zmtp=debug` on the Rust side.
