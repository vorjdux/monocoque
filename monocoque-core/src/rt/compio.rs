//! compio backend: native io_uring. Selected by `runtime-compio`.
//!
//! See [`super`](crate::rt) for the facade contract this implements.

use std::future::Future;

pub use compio::net::{TcpListener, TcpStream, ToSocketAddrsAsync as ToSocketAddrs};
pub use compio::time::{sleep, timeout};

/// Owned read half of a split TCP stream.
pub type OwnedReadHalf = compio::net::OwnedReadHalf<TcpStream>;
/// Owned write half of a split TCP stream.
pub type OwnedWriteHalf = compio::net::OwnedWriteHalf<TcpStream>;

#[cfg(unix)]
pub use compio::net::{UnixListener, UnixStream};

/// Handle to a spawned task. Kept alive keeps the task running; dropping it
/// detaches under compio.
pub type JoinHandle<T> = compio::runtime::Task<T>;

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
    compio::runtime::spawn_blocking(f).await
}

/// Await a spawned task and return its output.
///
/// Normalizes the difference between backends: compio's task awaits directly
/// to the output, so this is just the await.
#[inline]
pub async fn join<T>(handle: JoinHandle<T>) -> T {
    handle.await
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
