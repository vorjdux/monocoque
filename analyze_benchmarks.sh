#!/bin/bash
# Aggregate Criterion benchmark results

echo "# Monocoque Benchmark Results Summary"
echo ""
echo "Generated: $(date)"
echo ""

echo "## 🚀 Pipelined Throughput (with Batching API)"
echo ""

for file in target/criterion/pipelined_monocoque_dealer_router/*/new/estimates.json; do
    if [ -f "$file" ]; then
        size=$(echo $file | sed 's|.*/\([0-9]*\)/new.*|\1|')
        mean=$(jq -r '.mean.point_estimate' "$file")
        mean_ms=$(echo "scale=2; $mean / 1000000" | bc)
        echo "- **${size}B messages**: ${mean_ms} ms"
    fi
done

echo ""
echo "## Latency Comparison"
echo ""
echo "### Monocoque"
for file in target/criterion/latency_monocoque_req_rep/round_trip/*/new/estimates.json; do
    if [ -f "$file" ]; then
        size=$(echo $file | sed 's|.*/\([0-9]*B\)/new.*|\1|')
        mean=$(jq -r '.mean.point_estimate' "$file")
        mean_us=$(echo "scale=2; $mean / 1000" | bc)
        echo "- **${size}**: ${mean_us} μs"
    fi
done

echo ""
echo "### rust-zmq (zmq crate)"
for file in target/criterion/latency_rust_zmq_req_rep/round_trip/*/new/estimates.json; do
    if [ -f "$file" ]; then
        size=$(echo $file | sed 's|.*/\([0-9]*B\)/new.*|\1|')
        mean=$(jq -r '.mean.point_estimate' "$file")
        mean_us=$(echo "scale=2; $mean / 1000" | bc)
        echo "- **${size}**: ${mean_us} μs"
    fi
done

echo ""
echo "## IPC vs TCP Comparison"
echo ""
for type in ipc tcp; do
    echo "### ${type^^}"
    for file in target/criterion/ipc_vs_tcp_monocoque_${type}_throughput/*/new/estimates.json; do
        if [ -f "$file" ]; then
            size=$(echo $file | sed 's|.*/\([0-9]*\)/new.*|\1|')
            mean=$(jq -r '.mean.point_estimate' "$file")
            mean_ms=$(echo "scale=2; $mean / 1000000" | bc)
            echo "- **${size}B**: ${mean_ms} ms"
        fi
    done
    echo ""
done
