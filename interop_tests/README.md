# libzmq Interoperability Tests

This directory contains tests to verify wire-protocol compatibility between Monocoque and libzmq.

## Overview

These tests ensure that:
- Monocoque can communicate with libzmq-based applications
- The ZMTP protocol implementation is correct
- Security mechanisms (PLAIN, CURVE) work across implementations
- All socket patterns (REQ/REP, PUB/SUB, DEALER/ROUTER) are compatible

## Test Architecture

```
┌─────────────┐         ZMTP          ┌─────────────┐
│  Monocoque  │ ◄────────────────────► │   libzmq    │
│   (Rust)    │    Wire Protocol      │  (Python)   │
└─────────────┘                        └─────────────┘
```

## Prerequisites

```bash
# Install Python dependencies
pip install pyzmq pytest pytest-asyncio

# Ensure libzmq is installed
python -c "import zmq; print(f'libzmq version: {zmq.zmq_version()}')"
```

## Test Categories

### 1. Basic Patterns
- `test_req_rep_interop.py` - REQ/REP pattern
- `test_pub_sub_interop.py` - PUB/SUB pattern  
- `test_dealer_router_interop.py` - DEALER/ROUTER pattern

### 2. Security
- `test_plain_interop.py` - PLAIN authentication
- `test_curve_interop.py` - CURVE encryption

### 3. Edge Cases
- `test_multipart_interop.py` - Multipart messages
- `test_large_messages_interop.py` - Large message handling
- `test_reconnection_interop.py` - Connection recovery

## Running Tests

### Run all interop tests:
```bash
cd interop_tests
pytest -v
```

### Run specific pattern:
```bash
pytest test_req_rep_interop.py -v
```

### Run with Monocoque server:
```bash
# Terminal 1: Start Rust server
cargo run --example interop_server

# Terminal 2: Run Python client tests
pytest test_*_client.py -v
```

### Run with libzmq server:
```bash
# Terminal 1: Start Python server
python libzmq_server.py

# Terminal 2: Run Rust client tests
cargo test --test interop_client
```

## Test Matrix

| Pattern       | Monocoque→libzmq | libzmq→Monocoque | PLAIN | CURVE |
|---------------|------------------|------------------|-------|-------|
| REQ/REP       | ✅               | ✅               | ✅    | ✅    |
| PUB/SUB       | ✅               | ✅               | ❌    | ❌    |
| DEALER/ROUTER | ✅               | ✅               | ✅    | ✅    |
| PUSH/PULL     | 🔲               | 🔲               | ❌    | ❌    |

## Expected Results

All tests should pass with:
- Identical message payloads
- Correct message framing
- Proper authentication/encryption
- No protocol errors

## Troubleshooting

### Connection refused
- Check port bindings (0.0.0.0 vs 127.0.0.1)
- Verify firewall settings
- Ensure server is fully started

### Authentication failures
- Verify credentials match exactly
- Check CURVE key formats (Z85 encoding)
- Confirm ZAP handler is running

### Message corruption
- Enable debug logging: `RUST_LOG=monocoque_zmtp=debug`
- Capture network traffic: `tcpdump -i lo port 5555 -w capture.pcap`
- Compare wire protocol with libzmq reference

## References

- ZeroMQ Guide: https://zguide.zeromq.org/
- ZMTP Specification: https://rfc.zeromq.org/spec/23/
- PyZMQ Documentation: https://pyzmq.readthedocs.io/
