# monocoque-fuzz

`cargo-fuzz` (libFuzzer) targets for the wire-facing parsers and codecs. This
crate is excluded from the workspace and built with a nightly toolchain.

## Targets

| Target | Exercises |
| --- | --- |
| `fuzz_decoder` | streaming ZMTP frame decoder (`ZmtpCodec::decode` in `monocoque-zmtp/src/wire.rs`) fed in small chunks |
| `fuzz_frame_codec` | one-shot decode plus an `encode_plain`/decode round-trip |
| `fuzz_greeting` | 64-byte ZMTP greeting parser (`ZmtpGreeting::parse`) |
| `fuzz_command` | READY command parser and its property lengths (`parse_ready_command`) |
| `fuzz_curve_handshake` | CURVE key construction, X25519 agreement and the MESSAGE classifier (not the handshake commands) |
| `fuzz_security_plain` | ZAP request/response decoding and PLAIN ZAP request encoding |
| `fuzz_zap_request` | ZAP request/response frame decoders |
| `fuzz_subscription_trie` | subscription prefix trie |
| `fuzz_curve_decode` | CURVE MESSAGE decryption in the production codec, on raw and on corrupted encrypted frames |

Every target must only ever return `Ok`/`Err` (never panic) on arbitrary input.

## Running

```sh
# Build all targets (what CI gates on every PR)
cargo +nightly fuzz build --fuzz-dir monocoque-fuzz

# Run one target for a bounded time
cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_greeting -- -max_total_time=60

# Minimize a corpus to its coverage-unique inputs
cargo +nightly fuzz cmin --fuzz-dir monocoque-fuzz fuzz_greeting
```

`corpus/` holds committed seed inputs for each target. `artifacts/` (crash
reproducers) and `coverage/` are gitignored. See `docs/FUZZING.md` for triage.

## CI

- **Every PR**: `cargo fuzz build` (compile all targets).
- **Nightly + manual dispatch**: each target runs for 60s (`fuzz-run` job in
  `.github/workflows/ci.yml`).

## Follow-up: continuous fuzzing (OSS-Fuzz / ClusterFuzzLite)

Not yet set up. Two paths, both driven by a small `Dockerfile` + `build.sh` +
`project.yaml`:

- **OSS-Fuzz** (Google-hosted): apply via a PR to
  [`google/oss-fuzz`](https://github.com/google/oss-fuzz). Acceptance favors
  projects with a significant user base; revisit once monocoque is more widely
  used. Needs a maintainer contact email.
- **ClusterFuzzLite** (self-hosted in this repo's GitHub Actions): same build
  files, no external acceptance. Adds PR-diff fuzzing, corpus persistence, and
  crash artifacts on top of the scheduled `fuzz-run` job above. This is the
  natural next step when continuous fuzzing is wanted.
