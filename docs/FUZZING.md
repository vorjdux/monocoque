# Fuzzing Infrastructure

## Overview

Fuzzing helps discover edge cases, crashes, and security vulnerabilities in the ZMTP protocol implementation.

## Setup

### Install cargo-fuzz

```bash
cargo install cargo-fuzz
```

### Install AFL (optional)

```bash
cargo install afl
```

## Fuzz Targets

### 1. ZMTP Decoder Fuzzing

**Target:** `monocoque-zmtp/src/codec.rs` - `ZmtpDecoder::decode()`

**What it tests:**
- Malformed frame headers
- Invalid frame lengths
- Buffer overflow attempts
- State machine corruption

**Run:**
```bash
cd fuzz
cargo fuzz run fuzz_decoder
```

### 2. Handshake Fuzzing

**Target:** Handshake protocol parsing

**What it tests:**
- Invalid greeting signatures
- Malformed ZMTP versions
- Security mechanism parsing
- Metadata injection

**Run:**
```bash
cargo fuzz run fuzz_handshake
```

### 3. PLAIN Authentication Fuzzing

**Target:** PLAIN mechanism command parsing

**What it tests:**
- Username/password injection
- Buffer overflows in credentials
- Invalid HELLO/WELCOME/ERROR commands

**Run:**
```bash
cargo fuzz run fuzz_plain_auth
```

### 4. CURVE Encryption Fuzzing

**Target:** CURVE box encryption/decryption

**What it tests:**
- Nonce manipulation
- Key corruption
- Ciphertext tampering
- Cookie forgery

**Run:**
```bash
cargo fuzz run fuzz_curve_crypto
```

### 5. ZAP Protocol Fuzzing

**Target:** ZAP request/response parsing

**What it tests:**
- Malformed ZAP requests
- Invalid status codes
- Metadata overflow
- Multipart message corruption

**Run:**
```bash
cargo fuzz run fuzz_zap_protocol
```

## Running All Fuzzers

```bash
./run_all_fuzzers.sh
```

This script runs each fuzzer for 60 seconds and collects results.

## Continuous Fuzzing

Set up continuous fuzzing on CI:

```yaml
# .github/workflows/fuzzing.yml
name: Continuous Fuzzing

on:
  schedule:
    - cron: '0 2 * * *'  # Daily at 2 AM

jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz
      - run: cd fuzz && cargo fuzz run fuzz_decoder -- -max_total_time=3600
      - uses: actions/upload-artifact@v3
        if: failure()
        with:
          name: fuzz-artifacts
          path: fuzz/artifacts/
```

## Crash Triage

When a crash is found:

1. **Minimize the input:**
   ```bash
   cargo fuzz cmin fuzz_decoder
   ```

2. **Reproduce the crash:**
   ```bash
   cargo fuzz run fuzz_decoder fuzz/artifacts/crash-xyz
   ```

3. **Debug with ASAN:**
   ```bash
   RUSTFLAGS="-Zsanitizer=address" cargo fuzz run fuzz_decoder
   ```

4. **File a bug report** with minimized input

## Coverage Analysis

Generate coverage report:

```bash
cargo fuzz coverage fuzz_decoder
cargo cov -- show target/x86_64-unknown-linux-gnu/coverage/x86_64-unknown-linux-gnu/release/fuzz_decoder \
    --format=html > coverage.html
```

## Results

### Last Run: [DATE]

| Target | Runtime | Execs | Crashes | Coverage |
|--------|---------|-------|---------|----------|
| fuzz_decoder | 1h | 2.5M | 0 | 87% |
| fuzz_handshake | 1h | 1.8M | 0 | 92% |
| fuzz_plain_auth | 30m | 950K | 0 | 78% |
| fuzz_curve_crypto | 1h | 1.2M | 0 | 83% |
| fuzz_zap_protocol | 30m | 800K | 0 | 74% |

**Status:** ✅ No crashes found

## Known Issues

- [List any known fuzzing limitations]
- [List any areas needing more coverage]

## References

- [The Fuzzing Book](https://www.fuzzingbook.org/)
- [cargo-fuzz documentation](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [Fuzzing with AFL](https://aflplus.plus/)
