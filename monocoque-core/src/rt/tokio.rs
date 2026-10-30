//! tokio backend: a thin adapter implementing the `compio::io` traits over
//! tokio streams. Selected by `runtime-tokio`. See [`super`](crate::rt).

use compio_buf::{BufResult, IoBuf, IoBufMut, IoVectoredBuf};
// `AsyncRead` is consumed only inside the macro-generated impls below, which
// the unused-import lint does not attribute back to this import.
#[allow(unused_imports)]
use compio_io::{AsyncRead, AsyncWrite};
use std::future::{Future, poll_fn};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
// Bring tokio's write trait into scope under an alias so its poll methods are
// callable on concrete tokio streams without colliding with the compio
// `AsyncWrite` the adapters implement. Reads go through `read_into`, whose
// bound already carries the read methods.
use tokio::io::{AsyncWrite as TokioAsyncWrite, ReadBuf};

pub use tokio::net::ToSocketAddrs;
pub use tokio::time::{sleep, timeout};

/// Handle to a spawned task. Dropping it detaches under tokio.
pub type JoinHandle<T> = tokio::task::JoinHandle<T>;

/// Spawn a task on the current runtime and return its handle.
///
/// Tasks are spawned onto the local set so they may hold `!Send` state,
/// matching compio's thread-per-core model. The caller must therefore run on
/// a current-thread runtime inside a `LocalSet` (see [`LocalRuntime`]).
#[inline]
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    tokio::task::spawn_local(fut)
}

/// Spawn a task and let it run on its own, discarding the handle.
#[inline]
pub fn spawn_detached<F>(fut: F)
where
    F: Future + 'static,
    F::Output: 'static,
{
    drop(tokio::task::spawn_local(fut));
}

/// Run a blocking closure off the async executor and await its result.
///
/// Panics propagate, matching the compio backend where a panicking blocking
/// task aborts the await rather than yielding a value.
#[inline]
pub async fn spawn_blocking<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + Sync + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .expect("blocking task panicked")
}

/// Await a spawned task and return its output.
///
/// Normalizes the difference between backends: tokio's `JoinHandle` awaits to
/// a `Result`, so this unwraps the join error (a panicked or cancelled task)
/// to match compio, where the panic simply propagates through the await.
#[inline]
pub async fn join<T>(handle: JoinHandle<T>) -> T {
    handle
        .await
        .expect("spawned task panicked or was cancelled")
}

/// A self-contained runtime owned by a single thread.
///
/// For a thread that drives its own event loop, such as a test or a helper
/// thread that runs a socket outside the caller's runtime. A current-thread tokio runtime matches the single-threaded compio one and
/// lets `spawn`/`spawn_detached` run within `block_on`.
pub struct LocalRuntime {
    inner: tokio::runtime::Runtime,
    local: tokio::task::LocalSet,
}

impl LocalRuntime {
    /// Build a new single-threaded runtime with a local task set, so that
    /// `spawn`/`spawn_detached` can run `!Send` tasks within `block_on`.
    pub fn new() -> std::io::Result<Self> {
        let inner = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self {
            inner,
            local: tokio::task::LocalSet::new(),
        })
    }

    /// Run a future to completion on this runtime, blocking the thread.
    ///
    /// Drives the future and any locally spawned tasks to completion.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.local.block_on(&self.inner, fut)
    }
}

/// Read the spare capacity of an owned buffer from a tokio source.
///
/// Mirrors compio's owned-buffer read contract: bytes land in the buffer's
/// backing memory (no intermediate copy) and the count read is reported back
/// through `set_buf_init`. `ReadBuf::uninit` keeps this sound over the read
/// buffer's spare capacity, which is not yet initialized.
async fn read_into<R, B>(reader: &mut R, buf: B) -> BufResult<usize, B>
where
    R: tokio::io::AsyncRead + Unpin,
    B: IoBufMut,
{
    crate::io::fill_read(buf, async move |spare| {
        // `ReadBuf::uninit` reads straight into the buffer's backing memory
        // (no intermediate copy) and reports the count via `filled`.
        let mut read_buf = ReadBuf::uninit(spare);
        poll_fn(|cx| Pin::new(&mut *reader).poll_read(cx, &mut read_buf)).await?;
        Ok(read_buf.filled().len())
    })
    .await
}

/// Write the initialized bytes of an owned buffer to a tokio sink.
async fn write_from<W, B>(writer: &mut W, buf: B) -> BufResult<usize, B>
where
    W: tokio::io::AsyncWrite + Unpin,
    B: IoBuf,
{
    let slice = buf.as_init();
    let result = poll_fn(|cx| Pin::new(&mut *writer).poll_write(cx, slice)).await;
    match result {
        Ok(n) => BufResult(Ok(n), buf),
        Err(e) => BufResult(Err(e), buf),
    }
}

/// Write several owned buffers to a tokio sink in one `writev`.
///
/// `compio_io`'s default `write_vectored` issues one `send` per buffer; this
/// override coalesces them into a single `poll_write_vectored` syscall, matching
/// the compio and smol backends so a multi-frame send stays one syscall instead
/// of degrading to 2N (one per header and body).
async fn write_vectored_from<W, B>(writer: &mut W, buf: B) -> BufResult<usize, B>
where
    W: tokio::io::AsyncWrite + Unpin,
    B: IoVectoredBuf,
{
    // The `IoSlice`s borrow `buf`, which is held for the whole call.
    let result = poll_fn(|cx| {
        crate::io::with_vectored_slices(&buf, |slices| {
            Pin::new(&mut *writer).poll_write_vectored(cx, slices)
        })
    })
    .await;
    match result {
        Ok(n) => BufResult(Ok(n), buf),
        Err(e) => BufResult(Err(e), buf),
    }
}

/// Generate a compio-style stream adapter over a tokio I/O type.
///
/// The macro wires up the `compio::io` read/write traits plus the raw-fd
/// accessor the TCP tuning helpers rely on, so each concrete tokio type
/// (full stream, split halves, Unix variants) shares one implementation.
macro_rules! impl_compio_io {
    (read $ty:ty) => {
        impl AsyncRead for $ty {
            async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
                read_into(&mut self.inner, buf).await
            }
        }
    };
    (write $ty:ty) => {
        impl AsyncWrite for $ty {
            async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
                write_from(&mut self.inner, buf).await
            }

            async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
                write_vectored_from(&mut self.inner, buf).await
            }

            async fn flush(&mut self) -> io::Result<()> {
                poll_fn(|cx| Pin::new(&mut self.inner).poll_flush(cx)).await
            }

            async fn shutdown(&mut self) -> io::Result<()> {
                poll_fn(|cx| Pin::new(&mut self.inner).poll_shutdown(cx)).await
            }
        }
    };
    (raw_fd $ty:ty) => {
        #[cfg(unix)]
        impl std::os::unix::io::AsRawFd for $ty {
            fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
                self.inner.as_raw_fd()
            }
        }
    };
}

/// Implement the `compio::io` traits for `&$ty`, so one stream can have a read
/// and a write in flight at once through shared references, the way compio's
/// native streams do. tokio's `poll_read`/`poll_write` need `Pin<&mut _>`, so
/// these wait for readiness on `&self` and then issue the non-blocking
/// `try_*` call, retrying on a spurious wakeup.
macro_rules! impl_shared_io {
    ($ty:ty) => {
        impl AsyncRead for &$ty {
            async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
                let inner = &self.inner;
                crate::io::fill_read(buf, async move |spare| {
                    // Try first: under a ping-pong the bytes are usually
                    // already there, and registering readiness is the
                    // expensive part.
                    let len = spare.len();
                    loop {
                        let mut dst: &mut [std::mem::MaybeUninit<u8>] = &mut *spare;
                        match inner.try_read_buf(&mut dst) {
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                inner.readable().await?;
                            }
                            Ok(n) if n > 0 && n < len => {
                                // A short read drained the socket. Clear the
                                // cached readiness (a closure that reports
                                // WouldBlock does exactly that, with no
                                // syscall) so the next read waits for an
                                // event instead of spending a recv on EAGAIN.
                                // tokio's own poll_read does the same.
                                let _ = inner.try_io(tokio::io::Interest::READABLE, || {
                                    Err::<(), _>(io::ErrorKind::WouldBlock.into())
                                });
                                return Ok(n);
                            }
                            other => return other,
                        }
                    }
                })
                .await
            }
        }

        impl AsyncWrite for &$ty {
            async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
                let result = loop {
                    match self.inner.try_write(buf.as_init()) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            if let Err(e) = self.inner.writable().await {
                                break Err(e);
                            }
                        }
                        other => break other,
                    }
                };
                BufResult(result, buf)
            }

            async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
                let result = loop {
                    let attempt = crate::io::with_vectored_slices(&buf, |slices| {
                        self.inner.try_write_vectored(slices)
                    });
                    match attempt {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            if let Err(e) = self.inner.writable().await {
                                break Err(e);
                            }
                        }
                        other => break other,
                    }
                };
                BufResult(result, buf)
            }

            async fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }

            async fn shutdown(&mut self) -> io::Result<()> {
                socket2::SockRef::from(&self.inner).shutdown(std::net::Shutdown::Write)
            }
        }
    };
}

// ── TCP ──────────────────────────────────────────────────────────────────

/// Tokio TCP stream wearing the `compio::io` interface.
#[derive(Debug)]
pub struct TcpStream {
    inner: tokio::net::TcpStream,
}

/// Owned read half of a split [`TcpStream`].
#[derive(Debug)]
pub struct OwnedReadHalf {
    inner: tokio::net::tcp::OwnedReadHalf,
}

/// Owned write half of a split [`TcpStream`].
#[derive(Debug)]
pub struct OwnedWriteHalf {
    inner: tokio::net::tcp::OwnedWriteHalf,
}

impl TcpStream {
    /// Open a TCP connection to `addr`.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let inner = tokio::net::TcpStream::connect(addr).await?;
        Ok(Self { inner })
    }

    /// Adopt a `std::net::TcpStream`, attaching it to the current runtime.
    ///
    /// Mirrors compio's `TcpStream::from_std`. tokio requires the stream to be
    /// non-blocking, so this sets it.
    pub fn from_std(stream: std::net::TcpStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            inner: tokio::net::TcpStream::from_std(stream)?,
        })
    }

    /// Local address this stream is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// Address of the remote peer.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.inner.peer_addr()
    }

    /// Split into owned read and write halves.
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        let (r, w) = self.inner.into_split();
        (OwnedReadHalf { inner: r }, OwnedWriteHalf { inner: w })
    }
}

/// Tokio TCP listener returning [`TcpStream`] adapters.
#[derive(Debug)]
pub struct TcpListener {
    inner: tokio::net::TcpListener,
}

impl TcpListener {
    /// Bind a listening socket to `addr`.
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let inner = tokio::net::TcpListener::bind(addr).await?;
        Ok(Self { inner })
    }

    /// Adopt a `std::net::TcpListener`, attaching it to the tokio runtime.
    pub fn from_std(listener: std::net::TcpListener) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        Ok(Self {
            inner: tokio::net::TcpListener::from_std(listener)?,
        })
    }

    /// Accept the next inbound connection.
    pub async fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let (stream, addr) = self.inner.accept().await?;
        Ok((TcpStream { inner: stream }, addr))
    }

    /// Local address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// Bind a TCP listener with `SO_REUSEPORT`. See [`crate::tcp::reuseport_listener`].
pub fn bind_reuseport(addr: SocketAddr) -> io::Result<TcpListener> {
    TcpListener::from_std(crate::tcp::reuseport_listener(addr)?)
}

impl_compio_io!(read TcpStream);
impl_compio_io!(write TcpStream);
impl_compio_io!(raw_fd TcpStream);
impl_shared_io!(TcpStream);
// The split halves intentionally have no raw-fd accessor: TCP tuning is
// applied to the whole stream before it is split, and tokio's halves do not
// expose the descriptor.
impl_compio_io!(read OwnedReadHalf);
impl_compio_io!(write OwnedWriteHalf);

// ── Unix domain sockets ───────────────────────────────────────────────────

#[cfg(unix)]
pub use unix::{UnixListener, UnixStream};

#[cfg(unix)]
mod unix {
    use super::{
        AsyncRead, AsyncWrite, BufResult, IoBuf, IoBufMut, IoVectoredBuf, Pin, TokioAsyncWrite, io,
        poll_fn, read_into, write_from, write_vectored_from,
    };
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    /// Tokio Unix stream wearing the `compio::io` interface.
    #[derive(Debug)]
    pub struct UnixStream {
        inner: tokio::net::UnixStream,
    }

    impl UnixStream {
        /// Connect to a Unix domain socket at `path`.
        pub async fn connect<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            let inner = tokio::net::UnixStream::connect(path).await?;
            Ok(Self { inner })
        }

        /// Local (bound) address of this stream, if any.
        pub fn local_addr(&self) -> io::Result<tokio::net::unix::SocketAddr> {
            self.inner.local_addr()
        }

        /// Peer address of this stream, if any.
        pub fn peer_addr(&self) -> io::Result<tokio::net::unix::SocketAddr> {
            self.inner.peer_addr()
        }
    }

    /// Tokio Unix listener returning [`UnixStream`] adapters.
    #[derive(Debug)]
    pub struct UnixListener {
        inner: tokio::net::UnixListener,
    }

    impl UnixListener {
        /// Bind a Unix domain socket listener at `path`.
        ///
        /// Async to mirror the compio backend, where binding awaits; tokio's
        /// bind is synchronous, so this just wraps it.
        #[allow(clippy::unused_async)] // signature parity with the compio backend
        pub async fn bind<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            let inner = tokio::net::UnixListener::bind(path)?;
            Ok(Self { inner })
        }

        /// Accept the next inbound connection.
        pub async fn accept(&self) -> io::Result<(UnixStream, tokio::net::unix::SocketAddr)> {
            let (stream, addr) = self.inner.accept().await?;
            Ok((UnixStream { inner: stream }, addr))
        }

        /// Local address this listener is bound to.
        pub fn local_addr(&self) -> io::Result<tokio::net::unix::SocketAddr> {
            self.inner.local_addr()
        }
    }

    impl AsyncRead for UnixStream {
        async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
            read_into(&mut self.inner, buf).await
        }
    }

    impl AsyncWrite for UnixStream {
        async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            write_from(&mut self.inner, buf).await
        }

        // Without this override compio-io's default writes one iovec per call,
        // so a multi-frame send over IPC degraded to one syscall per header and
        // body while TCP took a single writev.
        async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
            write_vectored_from(&mut self.inner, buf).await
        }

        async fn flush(&mut self) -> io::Result<()> {
            poll_fn(|cx| Pin::new(&mut self.inner).poll_flush(cx)).await
        }

        async fn shutdown(&mut self) -> io::Result<()> {
            poll_fn(|cx| Pin::new(&mut self.inner).poll_shutdown(cx)).await
        }
    }

    impl AsRawFd for UnixStream {
        fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
            self.inner.as_raw_fd()
        }
    }

    impl_shared_io!(UnixStream);

    #[cfg(test)]
    mod tests {
        use super::{AsyncWrite, UnixStream};

        // compio-io's default write_vectored writes only the first non-empty
        // buffer per call. One call returning the full length proves the
        // override issues a single writev across every iovec.
        #[test]
        fn unix_write_vectored_is_one_writev() {
            crate::rt::LocalRuntime::new().unwrap().block_on(async {
                let (a, _b) = tokio::net::UnixStream::pair().unwrap();
                let mut a = UnixStream { inner: a };
                let bufs = [vec![1u8; 10], vec![2u8; 20], vec![3u8; 30]];
                let compio_buf::BufResult(n, _) = a.write_vectored(bufs).await;
                assert_eq!(n.unwrap(), 60);
            });
        }
    }
}
