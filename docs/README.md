# Monocoque Documentation

## Getting started

- [Getting Started](GETTING_STARTED.md) - Installation and first socket
- [User Guide](USER_GUIDE.md) - Socket patterns and common usage
- [Integration Guide](INTEGRATION_GUIDE.md) - Integrating into an existing project

## Migration

- [Migration from libzmq](MIGRATION.md) - API differences and how to port existing code

## Protocol and compatibility

- [Compatibility](COMPATIBILITY.md) - Which ZeroMQ features are supported
- [Interop Testing](INTEROP_TESTING.md) - Running tests against libzmq

## Security

- [Security Guide](SECURITY_GUIDE.md) - PLAIN, CURVE, and ZAP authentication
- [ZAP Integration](ZAP_INTEGRATION_GUIDE.md) - Implementing a custom ZAP handler

## Performance and operations

- [Performance](performance.md) - Benchmark results and tuning guide
- [Production Deployment](PRODUCTION_DEPLOYMENT.md) - OS tuning, monitoring, deployment checklist
- [Reliability](RELIABILITY_AND_RESILIENCE.md) - Reconnection, heartbeating, HWM

## Reference

- [Routing and Identity](IDENTITY_ROUTING_OPTIONS.md) - How ROUTER identity envelopes work
- [Fuzzing](FUZZING.md) - Fuzz targets and crash triage
- [Publishing](PUBLISHING.md) - Releasing to crates.io
- [Architecture Decisions](ADR.md) - Why key design choices were made

## Blueprints

Internal design documents covering the architecture in depth:
`blueprints/00-overview.md` through `blueprints/06-safety-model-and-unsafe-audit.md`
