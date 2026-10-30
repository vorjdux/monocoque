# Fuzzing

The fuzz targets live in `monocoque-fuzz/`, a `cargo-fuzz` (libFuzzer) crate
kept outside the workspace and built with a nightly toolchain. Each target
feeds arbitrary bytes to a parser or codec that a remote peer can reach. A
target passes when the code under test only ever returns `Ok` or `Err`; a
panic, an over-read, a hang or an out-of-memory is a finding.

## Setup

```bash
rustup toolchain install nightly
cargo install cargo-fuzz
```

## Targets

| Target | Code under test |
| --- | --- |
| `fuzz_decoder` | `ZmtpCodec::decode` in `monocoque-zmtp/src/wire.rs`, the streaming frame decoder every socket uses, fed in chunks of 1 to 64 bytes so partial headers and payloads are carried across calls |
| `fuzz_frame_codec` | one-shot `ZmtpCodec::decode` on raw input, plus an `encode_plain` then decode round trip |
| `fuzz_greeting` | `ZmtpGreeting::parse`, the 64-byte ZMTP greeting |
| `fuzz_command` | `parse_ready_command`, the READY command and its length-prefixed properties |
| `fuzz_curve_handshake` | CURVE key construction and conversion, X25519 key agreement, `CurveClient`/`CurveServer` construction and `CurveMessageCipher::is_curve_message` |
| `fuzz_security_plain` | `ZapRequest`/`ZapResponse` decoding, `ZapMechanism::from_str`, and PLAIN ZAP request encoding and its decode round trip |
| `fuzz_zap_request` | `ZapRequest::decode` and `ZapResponse::decode` on arbitrary frame counts and contents |
| `fuzz_subscription_trie` | `SubscriptionTrie` subscribe, unsubscribe and match sequences, and `SubscriptionEvent` parsing |
| `fuzz_curve_decode` | `ZmtpCodec::decode` with a CURVE cipher: raw bytes into the encrypted decoder, and client-encrypted frames decoded intact (must match) or with one corrupted byte (must not yield a different message) |

The CURVE handshake commands (HELLO, WELCOME, INITIATE, READY) are not fuzzed
yet.

## Running

Run these from the repository root.

```bash
# Build every target (CI does this on every PR)
cargo +nightly fuzz build --fuzz-dir monocoque-fuzz

# Run one target for 60 seconds
cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_decoder -- -max_total_time=60

# Run every target, 60 seconds each (10 by default)
scripts/run_fuzzer.sh 60

# Run named targets only
scripts/run_fuzzer.sh 30 fuzz_decoder fuzz_greeting
```

## CI

`.github/workflows/ci.yml` has two jobs:

- `fuzz-build` builds every target on each PR.
- `fuzz-run` runs each target for 60 seconds, on the nightly schedule and on
  manual dispatch only.

## Corpus and artifacts

`monocoque-fuzz/corpus/<target>/` holds committed seed inputs, so a fresh run
starts from useful cases. A run adds the inputs it finds there; commit only the
ones worth keeping, after minimizing:

```bash
cargo +nightly fuzz cmin --fuzz-dir monocoque-fuzz fuzz_greeting
```

Crash reproducers land in `monocoque-fuzz/artifacts/<target>/`, which is
ignored by git.

## Crash triage

1. Reproduce the crash:
   ```bash
   cargo +nightly fuzz run --fuzz-dir monocoque-fuzz fuzz_decoder \
       monocoque-fuzz/artifacts/fuzz_decoder/crash-<hash>
   ```
2. Shrink the input:
   ```bash
   cargo +nightly fuzz tmin --fuzz-dir monocoque-fuzz fuzz_decoder \
       monocoque-fuzz/artifacts/fuzz_decoder/crash-<hash>
   ```
3. Fix the bug and add a regression test with the shrunk input as a unit test
   next to the code. Do not commit the raw artifact.

## Coverage

```bash
cargo +nightly fuzz coverage --fuzz-dir monocoque-fuzz fuzz_decoder
```

This writes profile data under `monocoque-fuzz/coverage/fuzz_decoder/`, which
`llvm-cov show` can render against the instrumented binary.

## References

- [cargo-fuzz book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
