# Monocoque Documentation

## Getting started

- [Getting Started](GETTING_STARTED.md) - Installation and first socket
- [User Guide](USER_GUIDE.md) - Socket patterns and common usage
- [Integration Guide](INTEGRATION_GUIDE.md) - Integrating into an existing project

## Migration

- [Migration](MIGRATION.md) - porting from libzmq, and every breaking change since 0.4
- [RFC compliance](rfc-compliance.md) - what each ZeroMQ RFC covers here and where libzmq differs

## Protocol and compatibility

- [Compatibility](COMPATIBILITY.md) - Which ZeroMQ features are supported
- [Interop Testing](INTEROP_TESTING.md) - Running tests against libzmq

## Security

- [Security Guide](SECURITY_GUIDE.md) - PLAIN, CURVE, and ZAP authentication
- [ZAP Integration](ZAP_INTEGRATION_GUIDE.md) - Implementing a custom ZAP handler

## Performance and operations

- [Performance](performance.md) - Benchmark results and tuning guide
- [Production Deployment](PRODUCTION_DEPLOYMENT.md) - OS tuning, monitoring, deployment checklist
- [Reliability](RELIABILITY_AND_RESILIENCE.md) - HWM, cancellation safety, reconnection, heartbeats
- [Troubleshooting](troubleshooting.md) - Symptoms, their usual causes and fixes

## Reference

- [Routing and Identity](IDENTITY_ROUTING_OPTIONS.md) - How ROUTER identity envelopes work
- [Fuzzing](FUZZING.md) - Fuzz targets and crash triage
- [Publishing](PUBLISHING.md) - Releasing to crates.io
- [Architecture Decisions](ADR.md) - Why key design choices were made

## Blueprints

`blueprints/00-overview.md` through `blueprints/07-benchmark-gap-remediation-plan.md`
are historical design notes written before 1.0. They record how the design was
arrived at and name types and functions that no longer exist. The current
architecture is described in the crate documentation (`cargo doc`, or docs.rs)
and in the [User Guide](USER_GUIDE.md).
