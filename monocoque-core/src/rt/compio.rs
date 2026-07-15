//! compio backend: native `io_uring`. Selected by `runtime-compio`.
//!
//! See [`super`](crate::rt) for the facade contract this implements.

use std::future::Future;

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
/// Used by worker threads that drive their own event loop independently of
/// the caller's runtime (for example the publisher's fan-out workers).
pub struct LocalRuntime {
    inner: compio::runtime::Runtime,
}

impl LocalRuntime {
    /// Build a new single-threaded runtime.
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            inner: compio::runtime::Runtime::new()?,
        })
    }

    /// Run a future to completion on this runtime, blocking the thread.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.inner.block_on(fut)
    }
}
