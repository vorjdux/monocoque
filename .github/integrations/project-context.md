# Monocoque Development Context

## Project Overview

Monocoque is a high-performance, Rust-native ZeroMQ-compatible messaging runtime. It runs on `io_uring` (via `compio`) by default, with optional tokio and smol backends selected by the `runtime-tokio` and `runtime-smol` features.

Blueprints covering design decisions live in `docs/blueprints/`. `docs/MIGRATION.md` lists every API that changed from 0.4.

## Core Architecture

Start here:

- `docs/blueprints/00-overview.md` - System architecture
- `docs/blueprints/06-safety-model-and-unsafe-audit.md` - Safety model

Every socket is one multiplexer (`SocketCore`, in `monocoque-core/src/mux/`) over any number of peers. A socket is `!Send` and runs on the thread that created it.

- Each wire peer runs one persistent reader loop and one persistent writer loop (`mux/peer.rs`). Each inproc peer moves `Msg` values through a queue with no codec (`mux/inproc.rs`).
- There is no channel between the wire and the user. A complete message waits in its peer's ready queue until a receive takes it, and while the user waits, the user's own poll runs the reader loop that produces it (`mux/children.rs`).
- Background work (accepting, dialing, reconnecting, heartbeats, linger) runs on one driver task per socket (`mux/driver.rs`), only while the user is not polling.
- The core never knows the wire protocol. A `Protocol` supplies the handshake and a per-connection `Codec`. `monocoque-zmtp/src/wire.rs` implements it for ZMTP.
- The socket patterns (`monocoque-zmtp/src/sock/kinds.rs`, `sock/draft.rs`, `sock/stream.rs`) decide which peer a message goes to; the core routes nothing itself.

Rule for anything that touches a shared core: borrow, do the work, drop the borrow. Never hold the borrow across an `.await`.

## Messages

`Msg` (`monocoque-core/src/msg.rs`) is the one message type, an ordered list of frames stored by shape:

- one small frame (up to `INLINE_CAP` bytes) inline in the `Msg`, with no allocation or refcount;
- one larger frame as a single `Bytes`;
- up to four frames in a `SmallVec`;
- `Msg::into_shared` puts the frames behind one `Arc` so a fan-out to N peers costs one allocation and N refcount bumps.

`recv()` returns a `MsgRef<'_>` that borrows the socket's receive slot; `recv_owned()` returns a `Msg` that outlives it.

## Safety Rules

- `unsafe` lives only in `monocoque-core/src/io.rs` (declaring the bytes a read initialized) and `monocoque-core/src/tcp.rs` (socket option calls). The crate denies `unsafe_code` everywhere else. The README's Safety section is the authoritative inventory.
- Every `unsafe` block needs a `SAFETY:` comment stating why it is sound.
- Protocol, routing and pub/sub code is safe Rust.

## Protocol

`monocoque-zmtp` speaks ZMTP 3.1 and 3.0, and ZMTP 1.0 and 2.0 framing for legacy peers.

- `greeting.rs` and `handshake.rs`: greeting, version negotiation, READY with `Socket-Type` metadata (libzmq drops a peer whose READY lacks it).
- `security/`: NULL, PLAIN, CURVE (RFC 26, libzmq-compatible) and ZAP.
- `wire.rs`: the codec. It decodes straight out of a peer's read buffer into `Msg`s and writes large frames as their own iovec.
- `sock/`: the socket types. The draft types (CLIENT/SERVER, RADIO/DISH, SCATTER/GATHER, CHANNEL) sit behind the `draft` feature.

## Pub/Sub

`monocoque-core/src/subscription.rs` is the one matcher behind PUB, XPUB, SUB and XSUB: a lexicographically sorted prefix vector with per-peer counts, scanned forward with an early exit. `SubscriptionTrie` is the same index with a single subscriber, used for local filtering.

## Public API

Crate: `monocoque-rs` (library name `monocoque`). The ZeroMQ API is behind the opt-in `zmq` feature.

```rust
use monocoque::rt::LocalRuntime;
use monocoque::zmq::DealerSocket;

fn main() -> monocoque::zmq::Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut socket = DealerSocket::new();
        socket.connect("tcp://127.0.0.1:5555").await?;
        socket.send("hello").await?;
        let reply = socket.recv().await?;
        println!("{:?}", reply.get(0));
        Ok(())
    })
}
```

## Development Workflows

### Testing Strategy

1. **Unit tests**: deterministic logic in each crate
2. **Interop tests**: against real `libzmq` and pyzmq peers (`scripts/run_interop_tests.sh`)
3. **Stress and bound tests**: reconnection churn, fan-out, memory and wakeup bounds (`monocoque/tests/`)
4. **Fuzzing**: `monocoque-fuzz/`

Run tests per backend, since the backends are mutually exclusive:

```
cargo test --workspace --features zmq
cargo test --workspace --no-default-features --features runtime-tokio,zmq
cargo test --workspace --no-default-features --features runtime-smol,zmq
```

### Build Conventions

- Go through `monocoque_core::rt` for runtime types (streams, listeners, spawn, timers), never a runtime crate directly.
- Use `bytes` for frame payloads.
- Same-thread queues use `localq::LocalQueue` (no atomics); `flume` is used only where a value crosses threads (cross-thread inproc, socket monitors).
- No `Arc<Mutex<T>>` in hot paths.

## Key Files

```
monocoque/                 # Public API crate (monocoque-rs)
├── src/
│   ├── lib.rs             # Re-exports, rt facade
│   └── zmq/mod.rs         # monocoque::zmq namespace
├── examples/
├── benches/
└── tests/                 # Integration and interop tests

monocoque-zmtp/            # ZMTP implementation (internal)
└── src/
    ├── greeting.rs
    ├── handshake.rs
    ├── session.rs         # Socket types announced in READY
    ├── wire.rs            # ZMTP Protocol and Codec for the multiplexer
    ├── security/          # NULL, PLAIN, CURVE, ZAP
    └── sock/
        ├── mod.rs         # Socket<K>: bind, connect, options, timeouts
        ├── kinds.rs       # PAIR, DEALER, ROUTER, REQ, REP, PUSH, PULL, PUB, SUB, XPUB, XSUB
        ├── draft.rs       # Draft socket types (`draft` feature)
        ├── stream.rs      # STREAM
        ├── poll.rs        # zmq_poll
        └── proxy.rs       # zmq_proxy

monocoque-core/            # Protocol-agnostic kernel (internal)
└── src/
    ├── mux/               # SocketCore: peers, driver, inproc, child futures
    ├── msg.rs             # Msg, MsgRef
    ├── localq.rs          # Same-thread bounded queue
    ├── subscription.rs    # Subscription matcher
    ├── transport.rs       # TCP / IPC stream type
    ├── io.rs, tcp.rs      # The only unsafe code
    ├── rt/                # compio, tokio, smol backends
    ├── options.rs, endpoint.rs, monitor.rs, reconnect.rs, timeout.rs, error.rs
```

## CHANGELOG Maintenance

Update `CHANGELOG.md` when completing features, fixing bugs, making API changes, or improving performance. All entries go under `[Unreleased]` until publication. Use clear, user-facing language, not internal implementation details.
