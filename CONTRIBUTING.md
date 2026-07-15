# Contributing to Monocoque

Thanks for taking an interest. Monocoque is a ZeroMQ-compatible messaging runtime written in pure Rust, running on io_uring (via compio) by default with optional tokio and smol backends. It is young, so bug reports, interop findings, protocol coverage, docs, and performance work are all useful.

This guide covers the layout, how to build and test, what CI expects, and a few rules that keep the codebase coherent.

## Ways to contribute

- Report a bug or an interop mismatch against a real ZeroMQ peer.
- Improve protocol coverage or fix a wire-level edge case.
- Add or tighten tests, especially interop and fuzz targets.
- Improve docs and examples.
- Profile and improve performance, with numbers to back it.

If you are planning a larger change, open an issue first so we can agree on the direction before you spend time on it.

## Project layout

The workspace has three crates plus a separate fuzzing crate.

- `monocoque` is the public crate. It is the only API surface users depend on, and where the examples, benches, and interop tests live.
- `monocoque-core` is internal. It is the protocol-agnostic kernel: transports, buffers, backpressure, and the runtime facade (`rt`) that selects the io_uring, tokio, or smol I/O engine.
- `monocoque-zmtp` is internal. It is the ZMTP 3.1 implementation. Do not depend on it directly, the public API goes through `monocoque`.
- `monocoque-fuzz` holds the fuzz targets and is excluded from the workspace.

There is a PROJECT_STRUCTURE.md with more detail on the directory tree.

## Getting set up

You need a Rust toolchain at 1.95 or newer, which is the MSRV.

```
git clone https://github.com/vorjdux/monocoque
cd monocoque
cargo build --workspace
```

The interop tests talk to a real libzmq, so to run those you also need the library installed. On Debian or Ubuntu:

```
sudo apt-get install -y libzmq3-dev
```

## Building and testing

Run the test suite the way CI does. The three runtime backends are mutually
exclusive, so each is tested on its own rather than with `--all-features`. CI
runs a 3-backend matrix where each backend runs the full workspace:

```
cargo test --workspace --features zmq                                            # compio (default)
cargo test --workspace --no-default-features --features runtime-tokio,zmq        # tokio
cargo test --workspace --no-default-features --features runtime-smol,zmq         # smol
```

Interop tests against libzmq, which need the library installed above:

```
scripts/run_interop_tests.sh
```

Benchmarks, built on criterion:

```
scripts/bench_all.sh
```

Fuzzing needs a nightly toolchain and cargo-fuzz. The targets cover the decoder, the frame codec, the CURVE handshake, PLAIN security, and the subscription trie:

```
cargo install cargo-fuzz
scripts/run_fuzzer.sh
```

## What CI checks

A pull request needs to pass all of these, so it saves a round trip to run them locally first:

- Formatting: `cargo fmt --all -- --check`
- Lints on all three backends, with warnings treated as errors:
  `cargo clippy --workspace --all-targets --features zmq -- -D warnings`,
  `cargo clippy --workspace --all-targets --no-default-features --features runtime-tokio,zmq -- -D warnings`, and
  `cargo clippy --workspace --all-targets --no-default-features --features runtime-smol,zmq -- -D warnings`
- Tests on all three backends (see above)
- A build on the MSRV (1.95): `cargo build --workspace`
- Docs build with no warnings: `cargo doc --no-deps --workspace`
- A security audit of dependencies
- Fuzz targets build on nightly
- Interop tests against libzmq, from both Rust and pyzmq

The quick local check before pushing is fmt, clippy, and test.

## Architecture rules

A few invariants hold the design together. Please keep to them, or open an issue if you think one should change.

- Unsafe code is isolated to the allocator module in `monocoque-core`. Everything above it is safe Rust. Do not add `unsafe` elsewhere without a strong reason and a comment explaining why it is sound.
- The protocol codec is sans-io. The framing and greeting code parse bytes and produce frames without doing any I/O. Keep protocol logic and I/O separate, since that is what lets the core support more protocols later.
- The public API lives only in `monocoque`. Keep `monocoque-core` and `monocoque-zmtp` internal.
- Wire compatibility is the point of the project. Any change to protocol code must keep the libzmq interop tests passing. If you touch framing, run the interop suite before and after.

## Commit messages and pull requests

- Keep commits focused, one logical change each, and the history readable.
- Write the subject line in the imperative and keep it short, for example "fix dealer reconnect after peer drop". Put the why in the body when it is not obvious.
- Keep pull requests small and on one topic. A short note on what changed and why is enough.
- Run fmt, clippy, and the tests before you push.
- If your change affects the wire format or the public API, say so in the PR so it can go in the changelog.

## Reporting bugs

Open an issue with the smallest case that reproduces it: the socket pattern, the peer on the other side (Monocoque, libzmq, or pyzmq), and what you expected against what happened. Interop mismatches are most useful with a short snippet of both sides.

For anything that looks like a security issue, please contact the maintainer directly instead of opening a public issue.

## License

Monocoque is MIT licensed. By contributing, you agree that your contributions are licensed under the same terms.
