#!/usr/bin/env bash
# Run all libzmq interoperability examples

set -e  # Exit on error

echo "========================================"
echo "Monocoque ↔ libzmq Interop Test Suite"
echo "========================================"
echo ""

# Run each interop example
examples=(
    "interop_dealer_libzmq"
    "interop_router_libzmq"
    "interop_pubsub_libzmq"
)

failed=0

for example in "${examples[@]}"; do
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Running: $example"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    
    if cargo run --quiet --example "$example" --features zmq; then
        echo ""
        echo "✅ $example PASSED"
    else
        echo ""
        echo "❌ $example FAILED"
        failed=$((failed + 1))
    fi
    
    echo ""
done

echo "========================================"
echo "Summary"
echo "========================================"

if [ $failed -eq 0 ]; then
    echo "✅ All ${#examples[@]} interop tests passed!"
    exit 0
else
    echo "❌ $failed out of ${#examples[@]} tests failed"
    exit 1
fi
