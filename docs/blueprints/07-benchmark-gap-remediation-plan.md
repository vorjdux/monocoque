# 📄 File 7 - `07-benchmark-gap-remediation-plan.md`

# Benchmark Gap Remediation Plan

_Why the four benchmark gaps are worth fixing, grounded in the current write
and message paths, not guessed from the numbers._

---

## 1. Purpose

The benchmark suite (`monocoque/BENCHMARKS.md`) shows monocoque winning small
and mid-size message throughput on a single core, but four gaps remain:

1. A large-message throughput **plateau** (~2.9 GB/s) where multi-threaded
   libraries pull ahead at 4 KB / 16 KB.
2. An **8 B message gap** (~9 M msg/s vs `omq-compio`'s ~16 M).
3. **Absence from the PUB/SUB charts** entirely.
4. The single-core ceiling on the **large end**, where the field spends cores.

Before committing engineering effort, every proposed fix below was checked
against what the code does **today**. The diagnosis is from reading the paths,
not from a profile; item 3 of the plan exists precisely to close that gap
before spending the effort. Each claim is cited to `file:line`.

---

## 2. What the code does today (verified)

### 2.1 The write path copies, then does a plain `write()`

The hot send path encodes each frame **by copying its body** into a userspace
buffer, then issues a single `write_all`:

- `encode_multipart` copies every frame body with `buf.extend_from_slice(part)`
  at `monocoque-zmtp/src/codec.rs:293` (single-frame fast path) and
  `:319` (multi-frame path).
- The coalescing hot path calls that encoder into `send_buffer`, then flushes:
  `SocketBase::send_coalesced` (`monocoque-zmtp/src/base.rs:741`).
- The flush is a plain `stream.write_all(buf)` on the frozen buffer:
  `SocketBase::flush_send_buffer` (`monocoque-zmtp/src/base.rs:584-588`).
- The eager path is the same shape: `encode_message_to_write_buf` then
  `write_from_buf` then `stream.write_all(buf)`
  (`monocoque-zmtp/src/base.rs:651-659`), driven from
  `PushSocket::send` at `monocoque-zmtp/src/push.rs:99-100`.

There is **no vectored write and no zero-copy send anywhere** in the original
path: no `writev`, no `IORING_OP_SEND_ZC` / `MSG_ZEROCOPY`, no registered
buffers. compio is pinned at **0.10** (`Cargo.toml`,
`compio = { version = "0.10", ... }`). Verified against the pinned tree,
`AsyncWriteExt` **does** expose `write_vectored` / `write_vectored_all`
(compio-io 0.2.0) and `Bytes` implements `IoBuf`, so vectored writes are
available on 0.10 after all; only `IORING_OP_SEND_ZC` is genuinely missing.

**Consequence:** at 16 KB the per-message body copy into `send_buffer` is the
dominant per-byte cost on the core. The frame is `Bytes` (refcounted, already
owned), so copying it into the coalescing buffer only to hand the buffer to
`write_all` is a redundant memcpy that a vectored or zero-copy send removes.

### 2.2 No small-message inlining

Every frame is a heap `Bytes` with an atomic refcount, even an 8 B payload:

- `Message` stores `frames: Vec<Bytes>` (`monocoque-core/src/message.rs:38`).
- `push_str` / `push_empty` allocate via `Bytes::copy_from_slice` /
  `Bytes::new` (`monocoque-core/src/message.rs:85, 137`).
- The decoder produces `Bytes` payloads on both the zero-copy fast path and the
  reassembly slow path (`monocoque-zmtp/src/codec.rs:204, 143`).

There is no inline (VSM-style) storage for payloads under a threshold. At
~16 M msg/s the per-message allocation + atomic refcount traffic is a real
fraction of the budget. libzmq already inlines small messages, so this is a
known, contained win, not speculation.

### 2.3 The PUSH data path is single-threaded per socket

A PUSH socket drives one `compio` runtime cooperatively; there are no I/O worker
threads on the data path. The benchmarks construct one runtime per OS thread
explicitly (`compio::runtime::Runtime::new()` per `std::thread::spawn`, e.g.
`monocoque/benches/multithreaded.rs:109-111`), confirming that scaling across
cores today means **separate runtimes on separate threads**, not a multi-threaded
socket.

### 2.4 PubSocket has no message coalescing or flush

The worker-pool `PubSocket` broadcasts each message with **one `write_all` per
subscriber per message**: `send_encoded_to_stream` calls
`stream.write_all(data)` at `monocoque-zmtp/src/publisher.rs:387`. There is no
`send_buffer`, no coalesce threshold, no `flush()`; the path PUSH already has
(`base.rs:741`) is simply not wired in.

Nuance worth recording: `PubSocket` **already shards subscribers across worker
threads, each running its own compio runtime + io_uring** (`publisher.rs:391-394`).
So the multi-runtime-per-core model that item 5 proposes for PUSH already exists
for PUB; the missing piece on the PUB side is per-subscriber **syscall
amortization**, not threading.

---

## 3. Why each fix is useful (priority by impact-per-risk)

### Fix 1: Vectored / zero-copy writes for large frames _(direct fix for §2.1)_

Above a threshold (start at the write-buffer size), stop copying the body into
`send_buffer`. Write the frame header and the refcounted `Bytes` body as an
iovec:

- `writev` removes the userspace body copy.
- `IORING_OP_SEND_ZC` (`MSG_ZEROCOPY`) additionally removes the kernel-buffer
  copy and is the bigger TCP lever at large sizes.

**Why it matters:** this is the direct cause of the ~2.9 GB/s plateau (§2.1).
Removing the copy lets the plateau lift toward NIC / memory bandwidth, which is
how monocoque can beat the multi-threaded field **while staying single-threaded**,
i.e. on per-core efficiency, the axis it already wins.

**Dependency / risk:** compio 0.10 turned out to expose `write_vectored_all`
(§2.1), so the `writev` half needs no dependency change and is **implemented on
this branch** (see §5). Only `IORING_OP_SEND_ZC` remains gated: it requires a
custom io_uring op against 0.10 or a compio that exposes it, and the 0.19
`cfg_select` problem already flagged makes that a deliberate dependency call.
Settle that before chasing the last increment of zero-copy at very large sizes.

### Fix 2: Small-message inlining / VSM _(direct fix for §2.2)_

Store payloads under a threshold (try 64 B) inline in the message (no allocation,
no Arc traffic) on both send and receive. Pair it with:

- making the throughput hot loop use the existing `send_batch`
  (`monocoque-zmtp/src/push.rs:130`, one flush per N messages) rather than one
  `send().await` per tiny message;
- a symmetric **drain-N-per-await** on receive.

**Why it matters:** this closes the 8 B gap (§2.2, ~9 M vs ~16 M). At that rate
the alloc + refcount + per-`await` overhead is the cost, and the fix is contained
and proven (libzmq does exactly this). `send_batch` already exists, so half the
pairing is free.

### Fix 3: Profile before committing to Fix 1 or Fix 2

flamegraph the large-message PUSH path to confirm the `send_buffer` memcpy
(§2.1) saturates the core, and the 8 B path to confirm alloc + refcount +
per-`await` (§2.2) is the cost. The §2 diagnosis is from reading code, not a
profile. If the large-message profile is dominated by the copy, **Fix 1 alone
may close the plateau**; measure before spending the effort.

### Fix 4: PUB/SUB coalescing _(direct fix for §2.4)_

Wire the PUSH coalescing path (`base.rs:741`) into the worker-pool `PubSocket`
so fanout batches per subscriber before flushing. The zero-copy refcount fanout
is already done (`Bytes::clone` is an O(1) refcount bump, `publisher.rs:380`);
coalescing just amortizes the syscalls.

**Why it matters:** monocoque is absent from the PUB/SUB charts only because
this path doesn't exist (§2.4). Lower ceiling than Fix 1/2, but it is the
difference between competing and not appearing at all.

### Fix 5: Multicore for PUSH, _only if Fix 1 does not close the large end_

The least invasive way to match the multi-threaded field without breaking the
cancellation-safety story: **shard independent connections across N
single-threaded runtimes, one per core** (the model `PubSocket` already uses,
§2.4), so a multi-connection workload scales while each socket stays
single-threaded and the Drop-based cancellation model is untouched.

**Do not put a single socket's data path across threads**: that is where the
correctness story monocoque sells starts to crack. This is the biggest hammer
and the one most likely to erode what makes monocoque distinctive, so it is last
on purpose: a last resort, not a goal.

---

## 4. Honest read

Fixes 1 and 2 are the ones that could let monocoque beat the field **on the
terms it already wins on, per-core efficiency**, and they fit the design.
Fix 1 is gated on the compio decision, so settle that first. Fix 5 would raise
the ceiling the most but is also the one that could erode why monocoque is
interesting; treat it as a last resort.

| # | Fix | Closes | Risk | Gate |
|---|-----|--------|------|------|
| 1 | Vectored / zero-copy writes | ~2.9 GB/s plateau (§2.1) | Med | compio op decision |
| 2 | Small-message inlining + batch loop | 8 B gap (§2.2) | Low | none (`send_batch` exists) |
| 3 | Profile first | validates 1 & 2 | None | do before 1/2 |
| 4 | PUB/SUB coalescing | chart absence (§2.4) | Low | reuse PUSH path |
| 5 | Per-core connection sharding | large-end ceiling | High | only if 1 falls short |

---

## 5. Implementation status (this branch)

Fixes 1, 2, and 4 are implemented; Fix 3 (profiling) is a measurement step and
Fix 5 is deferred by request.

- **Fix 1: vectored writes (done).** New `vectored_write_threshold` option and
  `SocketBase::send_vectored`, which writes each frame as a header iovec + the
  refcounted body `Bytes` via `write_vectored_all`, with no copy into
  `send_buffer`. Headers are built into the reused `write_buf` and the iovec
  `Vec` is reused across calls (`SocketBase::iov`), so the hot path is
  allocation-free; `codec::write_frame_header` writes each header in place.
  Wired into `PushSocket::send`'s eager path; skipped for CURVE (the cipher
  rewrites the body regardless) and for coalesced mode. Covered by
  `test_push_pull_vectored_large_frame`. **Measured** (loopback, 4-core cloud
  Xeon): vectored loses below ~32 KB and wins ~1.1-1.3x at/above it, so the
  default threshold is **32 KB**, not 8 KB (see §6). `SEND_ZC` is **not**
  implemented (still gated on the compio op decision).
- **Fix 2: batch / drain hot path (partly done).** `PullSocket::recv_batch`
  drains a whole burst per `.await` (the receive-side counterpart to the
  existing `send_batch`), surfaced on the high-level socket too. Covered by
  `test_push_pull_recv_batch`. **Measured** (loopback): it did **not** beat a
  tight `recv()` loop here (~0.8x), because per-await scheduling is not the
  bottleneck on loopback; it is kept as an ergonomic API, not a default win.
  Full VSM inline-small-message storage is **not** done: it is a breaking change
  to the frame representation (`Vec<Bytes>` → an inline-or-heap `Frame` type
  across the whole send/recv API), its own dependency decision, left for a
  dedicated change.
- **Fix 4: PUB/SUB coalescing (done).** The worker-pool `PubSocket` now drains
  queued broadcasts into a batch (`MAX_COALESCE_MSGS` / `COALESCE_BYTE_LIMIT`)
  and writes each subscriber its matching messages in one `write_vectored_all`,
  keeping the plaintext fan-out zero-copy (shared `Bytes` clones). Non-broadcast
  commands pulled mid-drain are deferred and processed after the flush so
  command ordering holds. Covered by `test_pub_broadcast_coalescing_burst`.
- **Fix 5: per-core connection sharding (deferred).** Not started, by request.

---

## 6. Measured results

Loopback, release build, **4-core cloud Xeon @ 2.8 GHz (shared/throttled)**,
*not* the reference machine in `BENCHMARKS.md`, so treat these as relative, not
absolute. Harness: `monocoque/examples/bench_changes.rs`
(`cargo run --release --features zmq --example bench_changes`), best of 2 runs.

**Fix 1: vectored vs copy (PUSH/PULL eager, one message per `send`):**

| Frame size | copy | vectored | ratio |
|---|---|---|---|
| 16 KB | 1.86 GB/s | 1.28 GB/s | 0.69x |
| 32 KB | 1.65 GB/s | 2.10 GB/s | 1.27x |
| 64 KB | 1.33 GB/s | 1.68 GB/s | 1.26x |
| 128 KB | 1.64 GB/s | 1.75 GB/s | 1.06x |
| 256 KB | 1.82 GB/s | 2.22 GB/s | 1.22x |
| 1 MB | 1.24 GB/s | 1.48 GB/s | 1.19x |

The crossover is ~32 KB: below it the contiguous copy + single `write` beats a
two-segment `writev`; at/above it skipping the copy wins ~1.1-1.3x. Removing the
per-call allocations (reused `write_buf` headers + reused iovec `Vec`) moved the
crossover down from ~256 KB to ~32 KB. Hence the **32 KB default threshold**. On
a machine with real memory-bandwidth pressure the copy costs more and the
crossover should fall further; tune `vectored_write_threshold` per deployment.

**Fix 2: `recv_batch` vs `recv` (64 B, PUSH `send_batch(256)`):** 6.1 M vs
7.8 M msg/s (0.78x). No win on loopback; kept as an API (see §5).

**Fix 4: PUB->SUB delivered broadcast, 1 subscriber (coalescing on):** 64 B
~174 K msg/s, 1 KB ~161 K msg/s. This is the enablement (PUB previously did one
`write` per subscriber per message); a clean on/off A/B needs the coalescing cap
made runtime-tunable, which is left as follow-up.
