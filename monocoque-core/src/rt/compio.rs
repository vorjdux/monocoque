//! compio backend: native `io_uring`. Selected by `runtime-compio`.
//!
//! See [`super`](crate::rt) for the facade contract this implements.

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
}

impl LocalRuntime {
    /// Build a new single-threaded runtime.
    ///
    /// A ring needs physically contiguous kernel memory, so with many rings
    /// alive and memory fragmented `io_uring_setup` can fail with `ENOMEM`.
    /// Rather than fail, this retries with smaller rings: a socket keeps only
    /// a few operations in flight per peer, and ring size does not change the
    /// cost of an operation.
    pub fn new() -> std::io::Result<Self> {
        let mut result = compio::runtime::Runtime::new();
        for entries in [256, 64] {
            match &result {
                Err(e) if e.kind() == std::io::ErrorKind::OutOfMemory => {
                    let mut proactor = compio::driver::ProactorBuilder::new();
                    proactor.capacity(entries);
                    result = compio::runtime::RuntimeBuilder::new()
                        .with_proactor(proactor)
                        .build();
                }
                _ => break,
            }
        }
        Ok(Self { inner: result? })
    }

    /// Run a future to completion on this runtime, blocking the thread.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        let spin = spin_budget();
        if spin.is_zero() {
            return self.inner.block_on(fut);
        }
        let rt = &self.inner;
        rt.enter(|| {
            let flag = Arc::new(WokenFlag {
                woken: AtomicBool::new(true),
                driver: rt.waker(),
            });
            let waker = Waker::from(Arc::clone(&flag));
            let mut cx = Context::from_waker(&waker);
            let mut fut = std::pin::pin!(fut);
            let mut backoff = SpinBackoff { misses: 0, skip: 0 };
            loop {
                if flag.woken.swap(false, Ordering::Acquire)
                    && let Poll::Ready(out) = fut.as_mut().poll(&mut cx)
                {
                    rt.run();
                    return out;
                }
                if rt.run() {
                    rt.poll_with(Some(Duration::ZERO));
                    continue;
                }
                if !backoff.should_spin() {
                    rt.poll();
                    continue;
                }
                // Nothing runnable. Reap completions without entering the
                // kernel's wait for a while: sleeping in io_uring_enter and
                // being woken costs more than a round trip on a local socket.
                // Yielding lets a peer that shares this core run meanwhile.
                let deadline = Instant::now() + spin;
                loop {
                    rt.poll_with(Some(Duration::ZERO));
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

/// Default for how long [`LocalRuntime::block_on`] polls for completions
/// before it sleeps. On a Unix socket REQ/REP round trip (64 B, two cores)
/// 5 µs and 20 µs measured alike, 7.0 µs against 10.0 without spinning;
/// 20 µs leaves room for a slower peer.
const DEFAULT_SPIN: Duration = Duration::from_micros(20);

/// The spin budget: `MONOCOQUE_SPIN_US` if set (0 disables), otherwise
/// [`DEFAULT_SPIN`], or none when this thread may run on one CPU only, where
/// spinning only delays the peer it waits for.
fn spin_budget() -> Duration {
    if let Some(us) = std::env::var("MONOCOQUE_SPIN_US")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_micros(us);
    }
    if single_cpu() {
        Duration::ZERO
    } else {
        DEFAULT_SPIN
    }
}

/// `true` when this thread has one CPU to run on. On Linux the count honours
/// the affinity mask and the cgroup CPU quota.
fn single_cpu() -> bool {
    std::thread::available_parallelism().map_or(true, |n| n.get() <= 1)
}

/// Stops spinning while it keeps finding nothing (an idle socket, or a peer
/// starved of CPU), and tries again after a while.
struct SpinBackoff {
    misses: u32,
    skip: u32,
}

impl SpinBackoff {
    /// Spins that find nothing, in a row, before spinning pauses.
    const MAX_MISSES: u32 = 8;
    /// Sleeps without spinning before trying again.
    const PAUSE: u32 = 64;

    fn should_spin(&mut self) -> bool {
        if self.skip > 0 {
            self.skip -= 1;
            return false;
        }
        true
    }

    fn hit(&mut self) {
        self.misses = 0;
    }

    fn miss(&mut self) {
        self.misses += 1;
        if self.misses >= Self::MAX_MISSES {
            self.misses = 0;
            self.skip = Self::PAUSE;
        }
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
