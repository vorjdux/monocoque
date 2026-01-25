#!/usr/bin/env python3
"""
libzmq Throughput Benchmark

Measures messages/second for REQ/REP pattern using libzmq.
Results can be compared with Monocoque benchmarks.
"""

import zmq
import time
import sys
import statistics


def bench_req_rep_throughput(message_size: int, num_messages: int = 10000):
    """Benchmark REQ/REP throughput"""
    
    ctx = zmq.Context()
    
    # Create REP server
    rep = ctx.socket(zmq.REP)
    rep.bind("tcp://127.0.0.1:0")
    endpoint = rep.getsockopt(zmq.LAST_ENDPOINT).decode()
    port = int(endpoint.split(":")[-1])
    
    # Create REQ client
    req = ctx.socket(zmq.REQ)
    req.connect(f"tcp://127.0.0.1:{port}")
    
    # Prepare message
    msg = b"X" * message_size
    
    # Warm-up
    for _ in range(100):
        req.send(msg)
        rep.recv()
        rep.send(msg)
        req.recv()
    
    # Benchmark
    start = time.time()
    
    for _ in range(num_messages):
        req.send(msg)
        rep.recv()
        rep.send(msg)
        req.recv()
    
    elapsed = time.time() - start
    
    # Calculate metrics
    throughput = num_messages / elapsed
    latency_us = (elapsed / num_messages) * 1_000_000
    
    req.close()
    rep.close()
    ctx.term()
    
    return {
        "throughput": throughput,
        "latency_us": latency_us,
        "elapsed": elapsed,
        "num_messages": num_messages
    }


def bench_pub_sub_throughput(message_size: int, num_messages: int = 100000):
    """Benchmark PUB/SUB throughput"""
    
    ctx = zmq.Context()
    
    # Create PUB server
    pub = ctx.socket(zmq.PUB)
    pub.bind("tcp://127.0.0.1:0")
    endpoint = pub.getsockopt(zmq.LAST_ENDPOINT).decode()
    port = int(endpoint.split(":")[-1])
    
    # Create SUB client
    sub = ctx.socket(zmq.SUB)
    sub.connect(f"tcp://127.0.0.1:{port}")
    sub.setsockopt(zmq.SUBSCRIBE, b"")
    
    # Allow subscription to propagate
    time.sleep(0.5)
    
    # Prepare message
    msg = b"X" * message_size
    
    # Benchmark - send all messages
    start = time.time()
    
    for _ in range(num_messages):
        pub.send(msg, zmq.NOBLOCK)
    
    send_elapsed = time.time() - start
    
    # Receive all messages
    recv_start = time.time()
    received = 0
    
    while received < num_messages:
        try:
            sub.recv(zmq.NOBLOCK)
            received += 1
        except zmq.Again:
            time.sleep(0.001)
    
    recv_elapsed = time.time() - recv_start
    
    pub.close()
    sub.close()
    ctx.term()
    
    return {
        "send_throughput": num_messages / send_elapsed,
        "recv_throughput": num_messages / recv_elapsed,
        "total_elapsed": send_elapsed + recv_elapsed
    }


def main():
    print("=" * 60)
    print("libzmq Throughput Benchmark")
    print("=" * 60)
    
    # REQ/REP benchmarks
    print("\nREQ/REP Pattern:")
    print("-" * 60)
    
    for size in [64, 256, 1024, 10240]:
        results = bench_req_rep_throughput(size, num_messages=10000)
        print(f"\nMessage size: {size} bytes")
        print(f"  Throughput: {results['throughput']:,.0f} msg/s")
        print(f"  Latency:    {results['latency_us']:.2f} μs")
        print(f"  Elapsed:    {results['elapsed']:.3f} s")
    
    # PUB/SUB benchmarks
    print("\n\nPUB/SUB Pattern:")
    print("-" * 60)
    
    for size in [64, 256, 1024]:
        results = bench_pub_sub_throughput(size, num_messages=100000)
        print(f"\nMessage size: {size} bytes")
        print(f"  Send throughput: {results['send_throughput']:,.0f} msg/s")
        print(f"  Recv throughput: {results['recv_throughput']:,.0f} msg/s")
        print(f"  Total elapsed:   {results['total_elapsed']:.3f} s")
    
    print("\n" + "=" * 60)
    print(f"ZeroMQ version: {zmq.zmq_version()}")
    print(f"PyZMQ version: {zmq.pyzmq_version()}")
    print("=" * 60)


if __name__ == "__main__":
    main()
