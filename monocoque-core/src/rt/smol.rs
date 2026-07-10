//! smol backend: a `compio::io` adapter over smol's async-io streams.
//! Selected by `runtime-smol`. See [`super`](crate::rt).

use compio_buf::{BufResult, IoBuf, IoBufMut, IoVectoredBuf};
// `AsyncRead`/`AsyncWrite` are consumed only inside the trait impls below,
// which the unused-import lint does not attribute back to this import.
#[allow(unused_imports)]
use compio_io::{AsyncRead, AsyncWrite};
use smol::Async;
use socket2::SockRef;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

// smol's `connect`/`bind` take a resolved `SocketAddr`, so callers that pass
// a `&str`/`String` need synchronous resolution. `std::net::ToSocketAddrs`
// is the trait every existing call site already satisfies.
pub use std::net::ToSocketAddrs;

/// Resolve to a single socket address. Numeric endpoints (the common case)
/// resolve without blocking; this does not offload DNS.
fn resolve<A: ToSocketAddrs>(addr: A) -> io::Result<SocketAddr> {
    addr.to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no socket address resolved"))
}

/// Read the spare capacity of an owned buffer from a smol source.
///
/// Same role as the tokio backend's `read_into`: it implements compio's
/// owned-buffer read contract with no intermediate copy. The difference is
/// mechanical. tokio has a `MaybeUninit`-aware `ReadBuf`; `futures_io` does
/// not, so instead of casting the uninitialized capacity to `&mut [u8]`
/// (which would be UB) we wait for readability and let the kernel write
/// straight into the buffer via `recv`, handing it the buffer's
/// `&mut [MaybeUninit<u8>]` (sound to form over uninitialized memory).
async fn read_into<T, B>(stream: &Async<T>, buf: B) -> BufResult<usize, B>
where
    B: IoBufMut,
    for<'a> SockRef<'a>: From<&'a T>,
{
    crate::io::fill_read(buf, async move |spare| {
        // Wait for readability and let the kernel `recv` straight into the
        // buffer's backing memory, reporting the count it wrote.
        stream
            .read_with(|inner| SockRef::from(inner).recv(spare))
            .await
    })
    .await
}

/// Write the initialized bytes of an owned buffer to a smol sink.
async fn write_from<T, B>(stream: &Async<T>, buf: B) -> BufResult<usize, B>
where
    B: IoBuf,
    for<'a> SockRef<'a>: From<&'a T>,
{
    let result = stream
        .write_with(|inner| SockRef::from(inner).send(buf.as_slice()))
        .await;
    match result {
        Ok(n) => BufResult(Ok(n), buf),
        Err(e) => BufResult(Err(e), buf),
    }
}

/// Write several owned buffers in one `writev`.
///
/// `compio_io`'s default `write_vectored` issues one `send` per buffer; this
/// override coalesces them into a single vectored syscall, matching the
/// compio backend so the PUB fan-out's batched writes stay one syscall.
async fn write_vectored_from<T, B>(stream: &Async<T>, buf: B) -> BufResult<usize, B>
where
    B: IoVectoredBuf,
    for<'a> SockRef<'a>: From<&'a T>,
{
    let result = stream
        .write_with(|inner| {
            // The `IoSlice`s borrow `buf`, which is held for the whole call.
            crate::io::with_vectored_slices(&buf, |slices| {
                SockRef::from(inner).send_vectored(slices)
            })
        })
        .await;
    match result {
        Ok(n) => BufResult(Ok(n), buf),
        Err(e) => BufResult(Err(e), buf),
    }
}

// ── Task spawning ─────────────────────────────────────────────────────────

thread_local! {
    // A per-thread executor matches compio's thread-per-core model and lets
    // spawned `!Send` tasks run within `block_on`. `LocalExecutor::spawn`
    // and `run` both take `&self`, so no interior mutability is needed.
    static EXECUTOR: smol::LocalExecutor<'static> = const { smol::LocalExecutor::new() };
}

/// Handle to a spawned task.
///
/// Unlike compio and tokio, dropping a smol `Task` **cancels** it. All call
/// sites either detach explicitly ([`spawn_detached`]) or await via [`join`];
/// the one place that stores handles (`PullFanIn`) wants cancel-on-drop.
pub type JoinHandle<T> = smol::Task<T>;

/// Spawn a task on the current thread's executor and return its handle.
///
/// The task runs only while this thread is inside [`LocalRuntime::block_on`]
/// (which drives `executor.run`), matching tokio's `LocalSet` requirement.
#[inline]
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    EXECUTOR.with(|ex| ex.spawn(fut))
}

/// Spawn a task and let it run on its own, discarding the handle.
#[inline]
pub fn spawn_detached<F>(fut: F)
where
    F: Future + 'static,
    F::Output: 'static,
{
    // A smol `Task` cancels on drop, so `detach` is required to keep it alive.
    EXECUTOR.with(|ex| ex.spawn(fut).detach());
}

/// Run a blocking closure off the async executor and await its result.
#[inline]
pub async fn spawn_blocking<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + Sync + 'static,
    T: Send + 'static,
{
    smol::unblock(f).await
}

/// Await a spawned task and return its output.
///
/// Normalizes the difference between backends: a smol `Task` awaits directly
/// to the output, like compio, so this is just the await.
#[inline]
pub async fn join<T>(handle: JoinHandle<T>) -> T {
    handle.await
}

/// A self-contained runtime owned by a single thread.
///
/// Zero-sized: the executor lives in a thread-local. `block_on` drives that
/// executor so tasks spawned via [`spawn`]/[`spawn_detached`] make progress.
pub struct LocalRuntime;

impl LocalRuntime {
    /// Build a new single-threaded runtime handle.
    #[allow(clippy::unnecessary_wraps)] // signature parity with the other backends
    pub fn new() -> io::Result<Self> {
        Ok(Self)
    }

    /// Run a future to completion on this thread's executor, blocking the
    /// thread and driving any locally spawned tasks alongside it.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        EXECUTOR.with(|ex| smol::block_on(ex.run(fut)))
    }
}

// ── Timers ────────────────────────────────────────────────────────────────

/// Sleep for `dur`.
pub async fn sleep(dur: Duration) {
    smol::Timer::after(dur).await;
}

/// Error returned by [`timeout`] when the future did not complete in time.
///
/// Call sites match on `Err(_)`, so the concrete type only needs to exist to
/// keep the `Result` shape identical to the other backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// Run `fut` with a deadline, returning `Err(Elapsed)` if it fires first.
///
/// compio and tokio re-export their runtime's native `timeout`; smol has
/// none, so this composes one from a timer and `select`.
pub async fn timeout<F, T>(dur: Duration, fut: F) -> Result<T, Elapsed>
where
    F: Future<Output = T>,
{
    use futures::future::{Either, select};
    use std::pin::pin;
    // `select` polls the future before the timer, so a ready value wins a tie.
    match select(pin!(fut), pin!(smol::Timer::after(dur))).await {
        Either::Left((val, _)) => Ok(val),
        Either::Right(_) => Err(Elapsed),
    }
}

// ── TCP ──────────────────────────────────────────────────────────────────

/// smol TCP stream wearing the `compio::io` interface.
///
/// Arc-backed so [`into_split`](TcpStream::into_split) can hand out
/// independent read and write halves over the same socket.
#[derive(Clone, Debug)]
pub struct TcpStream {
    inner: Arc<Async<std::net::TcpStream>>,
}

/// Owned read half of a split [`TcpStream`].
#[derive(Debug)]
pub struct OwnedReadHalf {
    inner: Arc<Async<std::net::TcpStream>>,
}

/// Owned write half of a split [`TcpStream`].
#[derive(Debug)]
pub struct OwnedWriteHalf {
    inner: Arc<Async<std::net::TcpStream>>,
}

impl TcpStream {
    /// Open a TCP connection to `addr`.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let target = resolve(addr)?;
        let inner = Async::<std::net::TcpStream>::connect(target).await?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Local address this stream is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.get_ref().local_addr()
    }

    /// Address of the remote peer.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.get_ref().peer_addr()
    }

    /// Split into owned read and write halves sharing the socket.
    #[must_use]
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        (
            OwnedReadHalf {
                inner: self.inner.clone(),
            },
            OwnedWriteHalf { inner: self.inner },
        )
    }
}

/// smol TCP listener returning [`TcpStream`] adapters.
#[derive(Debug)]
pub struct TcpListener {
    inner: Async<std::net::TcpListener>,
}

impl TcpListener {
    /// Bind a listening socket to `addr`.
    ///
    /// Async to mirror the other backends; smol's bind is synchronous.
    #[allow(clippy::unused_async)] // signature parity with the compio backend
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let target = resolve(addr)?;
        let inner = Async::<std::net::TcpListener>::bind(target)?;
        Ok(Self { inner })
    }

    /// Accept the next inbound connection.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (stream, addr) = self.inner.accept().await?;
        Ok((
            TcpStream {
                inner: Arc::new(stream),
            },
            addr,
        ))
    }

    /// Local address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.get_ref().local_addr()
    }
}

impl AsyncRead for TcpStream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        read_into(&self.inner, buf).await
    }
}

impl AsyncWrite for TcpStream {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        write_from(&self.inner, buf).await
    }

    async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        write_vectored_from(&self.inner, buf).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        SockRef::from(self.inner.get_ref()).shutdown(std::net::Shutdown::Write)
    }
}

impl AsyncRead for OwnedReadHalf {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        read_into(&self.inner, buf).await
    }
}

impl AsyncWrite for OwnedWriteHalf {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        write_from(&self.inner, buf).await
    }

    async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        write_vectored_from(&self.inner, buf).await
    }

    async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        SockRef::from(self.inner.get_ref()).shutdown(std::net::Shutdown::Write)
    }
}

#[cfg(unix)]
impl std::os::unix::io::AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        std::os::unix::io::AsRawFd::as_raw_fd(self.inner.get_ref())
    }
}

// ── Unix domain sockets ───────────────────────────────────────────────────

#[cfg(unix)]
pub use unix::{UnixListener, UnixStream};

#[cfg(unix)]
mod unix {
    use super::{
        Arc, AsyncRead, AsyncWrite, BufResult, IoBuf, IoBufMut, IoVectoredBuf, SockRef, io,
        read_into, write_from, write_vectored_from,
    };
    use smol::Async;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::SocketAddr;
    use std::path::Path;

    type StdUnixStream = std::os::unix::net::UnixStream;
    type StdUnixListener = std::os::unix::net::UnixListener;

    /// smol Unix stream wearing the `compio::io` interface.
    #[derive(Clone, Debug)]
    pub struct UnixStream {
        inner: Arc<Async<StdUnixStream>>,
    }

    impl UnixStream {
        /// Connect to a Unix domain socket at `path`.
        pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            let inner = Async::<StdUnixStream>::connect(path).await?;
            Ok(Self {
                inner: Arc::new(inner),
            })
        }

        /// Local (bound) address of this stream, if any.
        pub fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().local_addr()
        }

        /// Peer address of this stream, if any.
        pub fn peer_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().peer_addr()
        }
    }

    /// smol Unix listener returning [`UnixStream`] adapters.
    #[derive(Debug)]
    pub struct UnixListener {
        inner: Async<StdUnixListener>,
    }

    impl UnixListener {
        /// Bind a Unix domain socket listener at `path`.
        ///
        /// Async to mirror the compio backend; smol's bind is synchronous.
        #[allow(clippy::unused_async)] // signature parity with the compio backend
        pub async fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            let inner = Async::<StdUnixListener>::bind(path)?;
            Ok(Self { inner })
        }

        /// Accept the next inbound connection.
        pub async fn accept(&self) -> io::Result<(UnixStream, SocketAddr)> {
            let (stream, addr) = self.inner.accept().await?;
            Ok((
                UnixStream {
                    inner: Arc::new(stream),
                },
                addr,
            ))
        }

        /// Local address this listener is bound to.
        pub fn local_addr(&self) -> io::Result<SocketAddr> {
            self.inner.get_ref().local_addr()
        }
    }

    impl AsyncRead for UnixStream {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            read_into(&self.inner, buf).await
        }
    }

    impl AsyncWrite for UnixStream {
        async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            write_from(&self.inner, buf).await
        }

        async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            write_vectored_from(&self.inner, buf).await
        }

        async fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> io::Result<()> {
            SockRef::from(self.inner.get_ref()).shutdown(std::net::Shutdown::Write)
        }
    }

    impl AsRawFd for UnixStream {
        fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
            self.inner.get_ref().as_raw_fd()
        }
    }
}
