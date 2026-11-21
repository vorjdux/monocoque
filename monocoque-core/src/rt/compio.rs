//! compio backend: native `io_uring`. Selected by `runtime-compio`.
//!
//! See [`super`](crate::rt) for the facade contract this implements.

use crate::spin::{MAX_SPIN, SpinBackoff, spin_budget};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

pub use compio::net::{TcpListener, TcpStream, ToSocketAddrsAsync as ToSocketAddrs};
pub use compio::time::{sleep, timeout};

/// Owned read half of a split TCP stream.
///
/// compio 0.19's `TcpStream::into_split` returns two owned `TcpStream`s that
/// share the underlying fd, so the read and write halves are the same type.
pub type OwnedReadHalf = TcpStream;
/// Owned write half of a split TCP stream. See [`OwnedReadHalf`].
pub type OwnedWriteHalf = TcpStream;

#[cfg(unix)]
pub use compio::net::{UnixListener, UnixStream};

/// Bind a TCP listener with `SO_REUSEPORT` so multiple acceptors can share one
/// port with in-kernel load balancing. See [`crate::tcp::reuseport_listener`].
///
/// # Errors
///
/// Returns an error if the socket cannot be created/bound or adopted.
pub fn bind_reuseport(addr: std::net::SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::from_std(crate::tcp::reuseport_listener(addr)?)
}

/// Handle to a spawned task. Kept alive keeps the task running; dropping it
/// detaches under compio.
pub type JoinHandle<T> = compio::runtime::JoinHandle<T>;

/// Spawn a task on the current runtime and return its handle.
#[inline]
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
{
    compio::runtime::spawn(fut)
}

/// Spawn a task and let it run on its own, discarding the handle.
#[inline]
pub fn spawn_detached<F>(fut: F)
where
    F: Future + 'static,
    F::Output: 'static,
{
    compio::runtime::spawn(fut).detach();
}

/// Run a blocking closure off the async executor and await its result.
#[inline]
pub async fn spawn_blocking<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + Sync + 'static,
    T: Send + 'static,
{
    compio::runtime::spawn_blocking(f)
        .await
        .expect("blocking task panicked")
}

/// Await a spawned task and return its output.
///
/// Normalizes the difference between backends: compio's `JoinHandle` now awaits
/// to `Result<T, JoinError>` (like tokio), so unwrap the join error here.
#[inline]
pub async fn join<T>(handle: JoinHandle<T>) -> T {
    handle
        .await
        .expect("spawned task panicked or was cancelled")
}

/// A self-contained runtime owned by a single thread.
///
/// For a thread that drives its own event loop, such as a test or a helper
/// thread that runs a socket outside the caller's runtime.
pub struct LocalRuntime {
    inner: compio::runtime::Runtime,
    spin: Duration,
}

impl LocalRuntime {
    /// Build a new single-threaded runtime.
    ///
    /// A ring needs physically contiguous kernel memory, so with many rings
    /// alive and memory fragmented `io_uring_setup` can fail with `ENOMEM`.
    /// Rather than fail, this retries with smaller rings: a socket keeps only
    /// a few operations in flight per peer, and ring size does not change the
    /// cost of an operation.
    ///
    /// The ring is set up so that the kernel hands a completion over the next
    /// time the thread enters it, instead of interrupting the thread to do so
    /// (`IORING_SETUP_COOP_TASKRUN`): a runtime that goes back to the ring
    /// after every turn has no use for the interrupt. A kernel before 5.19
    /// refuses the flag, and the ring is then set up without it.
    ///
    /// Handing completions over only when asked (`IORING_SETUP_DEFER_TASKRUN`)
    /// is not used: asking takes a wait for one event, and that wait is what
    /// [`block_on`](Self::block_on) goes out of its way not to make.
    pub fn new() -> std::io::Result<Self> {
        let out_of_memory = |r: &std::io::Result<_>| matches!(r, Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory);
        let mut cooperative = true;
        let mut result = Self::build(None, cooperative);
        if result.is_err() && !out_of_memory(&result) {
            cooperative = false;
            result = Self::build(None, cooperative);
        }
        for entries in [256, 64] {
            if !out_of_memory(&result) {
                break;
            }
            result = Self::build(Some(entries), cooperative);
        }
        Ok(Self {
            inner: result?,
            spin: spin_budget(None),
        })
    }

    /// Set how long [`block_on`](Self::block_on) keeps polling for
    /// completions before it sleeps.
    ///
    /// The default is 50 µs, or none when the thread may run on one CPU only.
    /// `Duration::ZERO` turns the polling off, which is the faster setting
    /// when the peer's thread shares no L3 cache with this one, or when this
    /// thread is pinned to one CPU inside a process that has several. A
    /// budget above one second is lowered to one second.
    #[must_use]
    pub const fn with_spin(mut self, spin: Duration) -> Self {
        self.spin = if spin.as_nanos() > MAX_SPIN.as_nanos() {
            MAX_SPIN
        } else {
            spin
        };
        self
    }

    fn build(entries: Option<u32>, cooperative: bool) -> std::io::Result<compio::runtime::Runtime> {
        let mut proactor = compio::driver::ProactorBuilder::new();
        if let Some(entries) = entries {
            proactor.capacity(entries);
        }
        if cooperative {
            proactor.coop_taskrun(true);
        }
        compio::runtime::RuntimeBuilder::new()
            .with_proactor(proactor)
            .build()
    }

    /// Run a future to completion on this runtime, blocking the thread.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        let spin = self.spin;
        let rt = &self.inner;
        rt.enter(|| {
            let flag = Arc::new(WokenFlag {
                woken: AtomicBool::new(true),
                driver: rt.waker(),
            });
            let waker = Waker::from(Arc::clone(&flag));
            let mut cx = Context::from_waker(&waker);
            let mut fut = std::pin::pin!(fut);
            let mut backoff = SpinBackoff::default();
            // Reap what has completed, without waiting. Asked for with a zero
            // timeout alone, the driver waits for one event, and the kernel
            // serves that by putting the thread to sleep and waking it with a
            // timer: 6 to 13 µs each time on an EPYC 7232P, about one and a
            // half times per batch of the pipelined bench. A driver that has
            // just been woken does not wait, and waking one that is awake
            // only sets a flag. A REQ/REP round trip went from 16.9 to 14.4 µs
            // there and from 6.8 to 5.7 µs on an i7-1355U.
            let driver = rt.waker();
            let poll_now = || {
                driver.wake_by_ref();
                rt.poll_with(Some(Duration::ZERO));
            };
            loop {
                if flag.woken.swap(false, Ordering::Acquire)
                    && let Poll::Ready(out) = fut.as_mut().poll(&mut cx)
                {
                    rt.run();
                    return out;
                }
                if rt.run() {
                    poll_now();
                    continue;
                }
                if spin.is_zero() || !backoff.should_spin() {
                    rt.poll();
                    continue;
                }
                // Nothing runnable. Reap completions without entering the
                // kernel's wait for a while: sleeping in io_uring_enter and
                // being woken costs more than a round trip on a local socket.
                // Yielding lets a peer that shares this core run meanwhile.
                let deadline = Instant::now() + spin;
                loop {
                    poll_now();
                    let more = rt.run();
                    if more || flag.woken.load(Ordering::Acquire) {
                        backoff.hit();
                        break;
                    }
                    if Instant::now() >= deadline {
                        backoff.miss();
                        rt.poll();
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        })
    }
}

/// The waker of the future `block_on` drives: records that it needs polling,
/// then wakes the driver in case the thread is asleep in it.
struct WokenFlag {
    woken: AtomicBool,
    driver: Waker,
}

impl Wake for WokenFlag {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Release);
        self.driver.wake_by_ref();
    }
}
