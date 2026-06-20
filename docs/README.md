# Monocoque Documentation

## Getting Started

- [Getting Started Guide](GETTING_STARTED.md) - Installation, basic setup, first socket
- [Integration Guide](INTEGRATION_GUIDE.md) - Integrating Monocoque into an existing project

## Protocol and Compatibility

- [Implementation Status](IMPLEMENTATION_STATUS.md) - Which features are complete
- [Compatibility](COMPATIBILITY.md) - ZeroMQ version compatibility matrix
- [ZMTP Protocol](ZMTP_PROTOCOL.md) - Wire protocol internals and frame format
- [Socket Patterns](SOCKET_PATTERNS.md) - REQ/REP, PUB/SUB, DEALER/ROUTER, and PUSH/PULL

## Security

- [ZAP Integration Guide](ZAP_INTEGRATION_GUIDE.md) - Authentication protocol (RFC 27), PLAIN and CURVE mechanisms, custom handlers
- [Security Audit](SECURITY_AUDIT.md) - Threat model, cryptographic review, attack surface analysis

## Performance and Operations

- [Performance Benchmarks](PERFORMANCE.md) - Latency and throughput results vs. libzmq
- [Production Deployment Guide](PRODUCTION_DEPLOYMENT.md) - Security configuration, tuning, monitoring, Kubernetes/Docker, migration from libzmq
- [Fuzzing Guide](FUZZING.md) - Fuzz targets, crash triage, continuous fuzzing setup

## Implementation Guides

- [Identity and Routing](IDENTITY_ROUTING_OPTIONS.md) - How ROUTER identity envelopes work
- [TCP Keepalive](IMPLEMENTATION_TCP_KEEPALIVE_REQ_MODES.md) - Keepalive options and REQ modes
- [InProc Transport](INPROC_IMPLEMENTATION.md) - In-process socket transport
- [MongoDB-Style API](MONGODB_STYLE_SOCKET_API.md) - Alternative socket API
- [Interop Testing](INTEROP_TESTING.md) - Running interop tests against libzmq

## Tests

Integration tests live in `tests/`. Interop tests (requiring libzmq and Python) live in `interop_tests/`. Run them with:

```bash
cargo test
cd interop_tests && pytest -v
```

## Benchmarks

```bash
python benchmarks/libzmq_throughput.py
cargo bench
```

## API Docs

```bash
cargo doc --open
```

Key modules: `monocoque::req`, `monocoque::rep`, `monocoque::pub`, `monocoque::sub`, `monocoque::dealer`, `monocoque::router`, `monocoque::security`.
