//! Allocation frugality gate for the single-frame hot path.
//!
//! A per-binary counting global allocator tallies every allocation (and
//! reallocation). After warming a connected PUSH/PULL pair to steady state, it
//! measures the allocations charged to a long window of `send_one` +
//! `recv_into` and asserts the count stays far below one-per-message.
//!
//! The hot path is designed to allocate nothing per message: `send_one` encodes
//! into a reused write buffer, and `recv_into`/`try_recv_into` decode into a
//! caller-owned buffer, reading through a reused slab that only reallocates once
//! every `READ_SLAB_SIZE`/`read_size` kernel reads. The residual allocations are
//! that amortized slab growth, which does not scale with message count.
//!
//! This gate is what catches a per-message allocation sneaking back into the
//! send or receive path, e.g. the write-path `Vec` that `send_one` exists to
//! avoid (`send(vec![..])` allocates a `Vec` on every call): that regression
//! would push the count to one-per-message and trip the assertion.
//!
//! `recv_into` is measured deliberately, never `recv`: `recv` allocates a fresh
//! `Vec<Bytes>` per message by design, so it can never be zero-alloc.

use bytes::Bytes;
use monocoque::rt::{LocalRuntime, TcpListener};
use monocoque::zmq::{PullSocket, PushSocket, SocketOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Gate-controlled counting: only the measured window increments the counter,
/// so warmup and harness allocations do not pollute the count.
static COUNTING: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: delegates every operation to the system allocator unchanged; the
// atomics only observe, they never touch the returned pointers.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) != 0 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) != 0 {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const ADDR: &str = "127.0.0.1:0";
const PAYLOAD: usize = 64;
const WARM: usize = 4_000;
const MEAS: usize = 200_000;

/// Ceiling for allocations charged to the measured window.
///
/// Measured over 200k messages: smol 438, tokio 449, compio 1469 (compio's
/// `io_uring` read path allocates per kernel-read, a per-read cost that does not
/// scale with message count; the others reflect the reused slab reallocating
/// once per `READ_SLAB_SIZE` bytes read). The ceiling is `MEAS / 50` = 4000:
/// ~2.7x over the worst backend so it will not flap on noise, yet 50x below the
/// 200k a reintroduced per-message allocation would produce.
const MAX_WINDOW_ALLOCS: usize = MEAS / 50;

#[test]
fn single_frame_hot_path_makes_no_per_message_allocation() {
    let (port_tx, port_rx) = mpsc::channel::<u16>();

    // Sender: connect once the listener is bound, then blast single frames via
    // the zero-alloc `send_one` path.
    let sender = thread::spawn(move || {
        let rt = LocalRuntime::new().unwrap();
        rt.block_on(async move {
            let port = port_rx.recv().unwrap();
            let mut push = PushSocket::connect_with_options(
                ("127.0.0.1", port),
                SocketOptions::default()
                    .with_buffer_sizes(16384, 16384)
                    .with_write_coalescing(true),
            )
            .await
            .unwrap();
            let payload = Bytes::from(vec![0u8; PAYLOAD]);
            for _ in 0..(WARM + MEAS) {
                push.send_one(payload.clone()).await.unwrap();
            }
            push.flush().await.unwrap();
        });
    });

    let rt = LocalRuntime::new().unwrap();
    rt.block_on(async move {
        let listener = TcpListener::bind(ADDR).await.unwrap();
        port_tx.send(listener.local_addr().unwrap().port()).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut pull = PullSocket::from_tcp_with_options(
            stream,
            SocketOptions::default().with_buffer_sizes(16384, 16384),
        )
        .await
        .unwrap();

        let mut buf: Vec<Bytes> = Vec::with_capacity(4);

        // Warm: grow the write/read/decode buffers and the caller buffer to
        // steady state so nothing here counts as hot-path growth.
        let mut warmed = 0usize;
        while warmed < WARM {
            if !pull.recv_into(&mut buf).await.unwrap() {
                break;
            }
            warmed += 1;
            while warmed < WARM && pull.try_recv_into(&mut buf).unwrap() {
                warmed += 1;
            }
        }

        // Measured window: only these allocations are counted.
        COUNTING.store(1, Ordering::Relaxed);
        let mut got = 0usize;
        while got < MEAS {
            if !pull.recv_into(&mut buf).await.unwrap() {
                break;
            }
            got += 1;
            while got < MEAS && pull.try_recv_into(&mut buf).unwrap() {
                got += 1;
            }
        }
        COUNTING.store(0, Ordering::Relaxed);

        let allocs = ALLOCS.load(Ordering::Relaxed);
        assert_eq!(got, MEAS, "received {got} of {MEAS} messages");
        assert!(
            allocs <= MAX_WINDOW_ALLOCS,
            "single-frame hot path allocated {allocs} times over {MEAS} messages \
             (ceiling {MAX_WINDOW_ALLOCS}). That is roughly one allocation per \
             {} messages; a per-message allocation on the send or receive path \
             would show ~{MEAS}. Something reintroduced per-message allocation.",
            MEAS / allocs.max(1),
        );
    });

    sender.join().unwrap();
}
