#!/bin/bash
# Run the pyzmq interop suite: REQ/REP, PUB/SUB and CURVE against the libzmq
# bundled with pyzmq.

set -e

# Activate the pyenv environment if there is one.
if command -v pyenv >/dev/null 2>&1; then
    eval "$(pyenv init -)"
    pyenv shell monocoque 2>/dev/null || true
fi

echo "[1/3] Checking dependencies..."
command -v python3 >/dev/null 2>&1 || { echo "Error: python3 not found"; exit 1; }
python3 -c "import pytest" 2>/dev/null || { echo "Error: pytest not found. Run: pip install pyzmq pytest pytest-timeout"; exit 1; }
python3 -c "import zmq" 2>/dev/null || { echo "Error: pyzmq not found. Run: pip install pyzmq"; exit 1; }
echo "OK: libzmq $(python3 -c 'import zmq; print(zmq.zmq_version())'), CURVE $(python3 -c 'import zmq; print(zmq.has("curve"))')"

# The tests run these debug builds from target/debug/examples.
echo "[2/3] Building the Rust peers..."
cd "$(dirname "$0")/.."
cargo build -q -p monocoque-rs --features zmq \
    --example simple_rep_server --example simple_req_client --example curve_echo

echo "[3/3] Running the suite..."
cd interop_tests
python3 -m pytest -v test_req_rep_interop.py test_pub_sub_interop.py test_curve_interop.py
echo "All interop tests passed"
