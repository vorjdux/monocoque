#!/bin/bash
# Run all interop tests

set -e

echo "=================================="
echo "Running Monocoque Interop Tests"
echo "=================================="

# Activate pyenv environment if available
if command -v pyenv >/dev/null 2>&1; then
    eval "$(pyenv init -)"
    pyenv shell monocoque 2>/dev/null || true
fi

# Check dependencies
echo -e "\n[1/5] Checking dependencies..."
command -v pytest >/dev/null 2>&1 || { echo "Error: pytest not found. Run: pip install pytest pytest-asyncio"; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "Error: python3 not found"; exit 1; }

python3 -c "import zmq" 2>/dev/null || { echo "Error: pyzmq not found. Run: pip install pyzmq"; exit 1; }
echo "✓ Dependencies OK (pyzmq version: $(python3 -c 'import zmq; print(zmq.zmq_version())'))"

# Build Rust examples (only the ones needed for tests)
echo -e "\n[2/5] Building Rust examples..."
cd "$(dirname "$0")/.."
cargo build -p monocoque-zmtp --example simple_rep_server --quiet
cargo build -p monocoque-zmtp --example simple_req_client --quiet
echo "✓ Examples built"

# Run REQ/REP tests
echo -e "\n[3/5] Running REQ/REP interop tests..."
cd interop_tests
pytest test_req_rep_interop.py -v || { echo "✗ REQ/REP tests failed"; exit 1; }
echo "✓ REQ/REP tests passed"

# Run PUB/SUB tests
echo -e "\n[4/5] Running PUB/SUB interop tests..."
pytest test_pub_sub_interop.py -v || { echo "✗ PUB/SUB tests failed"; exit 1; }
echo "✓ PUB/SUB tests passed"

# Summary
echo -e "\n[5/5] Test Summary"
echo "=================================="
echo "✓ All interop tests passed"
echo "✓ libzmq compatibility verified"
echo "=================================="
