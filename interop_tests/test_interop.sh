#!/bin/bash

echo "=== Starting Monocoque REP Server with debug logging ==="
echo ""

# Start server in background, capturing output
cargo run -p monocoque-zmtp --example debug_rep_server 2>&1 | tee server.log &
SERVER_PID=$!

echo "Server PID: $SERVER_PID"
echo "Waiting for server to start..."
sleep 3

echo ""
echo "=== Starting libzmq REQ Client ==="
echo ""

# Run Python client with timeout
timeout 5 python3 test_libzmq_client.py 2>&1 | tee client.log

CLIENT_EXIT=$?

echo ""
echo "Client exit code: $CLIENT_EXIT"

# Clean up
echo ""
echo "=== Cleaning up ==="
kill $SERVER_PID 2>/dev/null
wait $SERVER_PID 2>/dev/null

echo ""
echo "=== Server Log ==="
cat server.log

echo ""
echo "=== Client Log ==="
cat client.log

rm -f server.log client.log
