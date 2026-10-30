//! Byte-stream transports a socket's peers run over.
//!
//! [`Stream`] is one connected TCP or IPC stream. A socket can bind TCP and
//! connect IPC at once, so the peer table holds streams of either kind behind
//! one type. Reads and writes go through `&Stream`, which lets one peer keep a
//! read and a write in flight at the same time without splitting the stream.
//!
//! [`Listener`] accepts on a bound TCP or IPC endpoint. inproc does not appear
//! here: it never touches a byte stream (see `crate::localq`).

use crate::endpoint::Endpoint;
use crate::options::SocketOptions;
use crate::rt::{PeerAddress, TcpListener, TcpStream};
use compio_buf::{BufResult, IoBuf, IoBufMut, IoVectoredBuf};
use compio_io::{AsyncRead, AsyncWrite};
use std::io;

#[cfg(unix)]
use crate::ipc::IpcListener;
#[cfg(unix)]
use crate::rt::UnixStream;

/// A connected TCP or IPC byte stream.
#[derive(Debug)]
pub enum Stream {
    /// TCP connection.
    Tcp(TcpStream),
    /// Unix-domain (IPC) connection.
    #[cfg(unix)]
    Ipc(UnixStream),
}

impl Stream {
    /// Dial `endpoint`. inproc endpoints are rejected; they are not streams.
    pub async fn connect(endpoint: &Endpoint) -> io::Result<Self> {
        match endpoint {
            Endpoint::Tcp(addr) => Ok(Self::Tcp(TcpStream::connect(*addr).await?)),
            #[cfg(unix)]
            Endpoint::Ipc(path) => Ok(Self::Ipc(UnixStream::connect(path).await?)),
            Endpoint::Inproc(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inproc endpoints are not byte streams",
            )),
        }
    }

    /// Apply per-connection tuning from `options`.
    ///
    /// TCP gets `TCP_NODELAY` (always on, eager delivery is the contract) and
    /// keepalive. Both kinds get `SO_SNDBUF`/`SO_RCVBUF` when set: Unix sockets
    /// have no kernel autotuning, so on IPC these are the only knob there is.
    pub fn configure(&self, options: &SocketOptions) -> io::Result<()> {
        match self {
            Self::Tcp(s) => {
                crate::tcp::enable_tcp_nodelay(s)?;
                crate::tcp::configure_socket_buffers(s, options.sndbuf, options.rcvbuf)?;
                crate::tcp::configure_tcp_keepalive(
                    s,
                    options.tcp_keepalive,
                    options.tcp_keepalive_cnt,
                    options.tcp_keepalive_idle,
                    options.tcp_keepalive_intvl,
                )
            }
            #[cfg(unix)]
            Self::Ipc(s) => crate::tcp::configure_socket_buffers(s, options.sndbuf, options.rcvbuf),
        }
    }

    /// Close the write side, signalling end of stream to the peer.
    pub async fn shutdown(&self) -> io::Result<()> {
        let mut this = self;
        AsyncWrite::shutdown(&mut this).await
    }
}

impl From<TcpStream> for Stream {
    fn from(s: TcpStream) -> Self {
        Self::Tcp(s)
    }
}

#[cfg(unix)]
impl From<UnixStream> for Stream {
    fn from(s: UnixStream) -> Self {
        Self::Ipc(s)
    }
}

impl PeerAddress for Stream {
    fn zap_address(&self) -> String {
        match self {
            Self::Tcp(s) => s.zap_address(),
            #[cfg(unix)]
            Self::Ipc(s) => s.zap_address(),
        }
    }
}

impl PeerAddress for &Stream {
    fn zap_address(&self) -> String {
        (**self).zap_address()
    }
}

impl AsyncRead for &Stream {
    async fn read<B: IoBufMut>(&mut self, buf: B) -> BufResult<usize, B> {
        match *self {
            Stream::Tcp(s) => {
                let mut s = s;
                s.read(buf).await
            }
            #[cfg(unix)]
            Stream::Ipc(s) => {
                let mut s = s;
                s.read(buf).await
            }
        }
    }
}

/// On io_uring, a write the socket buffer can take right away is cheaper as a
/// plain non-blocking `send` than as a ring submission and completion: it
/// costs one syscall and no wakeup. `None` means the buffer is full (or the
/// call failed in a way the ring path should report), so submit to the ring.
#[cfg(all(feature = "runtime-compio", target_os = "linux"))]
fn send_now(stream: &Stream, bufs: &[io::IoSlice<'_>]) -> Option<usize> {
    use std::os::fd::AsFd;
    // Past this a coalesced batch is cheaper handed to the ring than copied
    // into the socket buffer inline (measured on PUSH/PULL 4 KiB coalesced).
    const SEND_NOW_MAX: usize = 64 * 1024;
    if bufs.iter().map(|b| b.len()).sum::<usize>() > SEND_NOW_MAX {
        return None;
    }
    const MSG_DONTWAIT: i32 = 0x40;
    const MSG_NOSIGNAL: i32 = 0x4000;
    let fd = match stream {
        Stream::Tcp(s) => s.as_fd(),
        Stream::Ipc(s) => s.as_fd(),
    };
    let sock = socket2::SockRef::from(&fd);
    match sock.send_vectored_with_flags(bufs, MSG_DONTWAIT | MSG_NOSIGNAL) {
        Ok(n) if n > 0 => Some(n),
        _ => None,
    }
}

impl AsyncWrite for &Stream {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        #[cfg(all(feature = "runtime-compio", target_os = "linux"))]
        if let Some(n) = send_now(self, &[io::IoSlice::new(buf.as_init())]) {
            return BufResult(Ok(n), buf);
        }
        match *self {
            Stream::Tcp(s) => {
                let mut s = s;
                s.write(buf).await
            }
            #[cfg(unix)]
            Stream::Ipc(s) => {
                let mut s = s;
                s.write(buf).await
            }
        }
    }

    async fn write_vectored<B: IoVectoredBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        #[cfg(all(feature = "runtime-compio", target_os = "linux"))]
        {
            let slices: smallvec::SmallVec<[io::IoSlice<'_>; 8]> =
                buf.iter_slice().map(io::IoSlice::new).collect();
            if let Some(n) = send_now(self, &slices) {
                drop(slices);
                return BufResult(Ok(n), buf);
            }
        }
        match *self {
            Stream::Tcp(s) => {
                let mut s = s;
                s.write_vectored(buf).await
            }
            #[cfg(unix)]
            Stream::Ipc(s) => {
                let mut s = s;
                s.write_vectored(buf).await
            }
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        match *self {
            Stream::Tcp(s) => {
                let mut s = s;
                s.shutdown().await
            }
            #[cfg(unix)]
            Stream::Ipc(s) => {
                let mut s = s;
                s.shutdown().await
            }
        }
    }
}

/// `true` if `err` means the process or system ran out of file descriptors
/// (EMFILE/ENFILE), the one accept failure worth backing off on.
#[must_use]
pub fn is_fd_exhaustion(err: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(err.raw_os_error(), Some(23 | 24))
    }
    #[cfg(not(unix))]
    {
        let _ = err;
        false
    }
}

/// A bound TCP or IPC listener.
#[derive(Debug)]
pub enum Listener {
    /// TCP listener.
    Tcp(TcpListener),
    /// IPC listener; unlinks its socket file on drop.
    #[cfg(unix)]
    Ipc(IpcListener),
}

impl Listener {
    /// Bind `endpoint`. Returns the listener and the endpoint actually bound,
    /// which differs from the request for a TCP port of 0.
    pub async fn bind(
        endpoint: &Endpoint,
        options: &SocketOptions,
    ) -> io::Result<(Self, Endpoint)> {
        match endpoint {
            Endpoint::Tcp(addr) => {
                let listener = if options.reuse_port {
                    crate::rt::bind_reuseport(*addr)?
                } else {
                    TcpListener::bind(*addr).await?
                };
                let bound = Endpoint::Tcp(listener.local_addr()?);
                Ok((Self::Tcp(listener), bound))
            }
            #[cfg(unix)]
            Endpoint::Ipc(path) => {
                let listener = crate::ipc::bind(path).await?;
                Ok((Self::Ipc(listener), endpoint.clone()))
            }
            Endpoint::Inproc(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inproc endpoints are not byte streams",
            )),
        }
    }

    /// Accept the next connection.
    pub async fn accept(&self) -> io::Result<Stream> {
        match self {
            Self::Tcp(l) => Ok(Stream::Tcp(l.accept().await?.0)),
            #[cfg(unix)]
            Self::Ipc(l) => Ok(Stream::Ipc(l.accept().await?.0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compio_buf::{IntoInner, IoBuf};

    #[cfg(unix)]
    #[test]
    fn fd_exhaustion_errnos_are_recognized() {
        // EMFILE (24) and ENFILE (23) are fd exhaustion.
        assert!(is_fd_exhaustion(&io::Error::from_raw_os_error(24)));
        assert!(is_fd_exhaustion(&io::Error::from_raw_os_error(23)));
    }

    #[test]
    fn other_errors_are_not_fd_exhaustion() {
        assert!(!is_fd_exhaustion(&io::Error::from_raw_os_error(104))); // ECONNRESET
        assert!(!is_fd_exhaustion(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "would block"
        )));
        // An error with no OS errno (e.g. a synthetic one) is not fd exhaustion.
        assert!(!is_fd_exhaustion(&io::Error::other("synthetic")));
    }

    #[test]
    fn concurrent_read_and_write_through_shared_refs() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (listener, bound) = Listener::bind(
                &Endpoint::parse("tcp://127.0.0.1:0").unwrap(),
                &SocketOptions::default(),
            )
            .await
            .unwrap();
            let accept = crate::rt::spawn(async move { listener.accept().await.unwrap() });
            let client = Stream::connect(&bound).await.unwrap();
            client.configure(&SocketOptions::default()).unwrap();
            let server = crate::rt::join(accept).await;

            // A read parked on the server while the client writes.
            let server = std::rc::Rc::new(server);
            let reader = {
                let server = std::rc::Rc::clone(&server);
                crate::rt::spawn(async move {
                    let mut s = &*server;
                    let BufResult(n, buf) = s.read(Vec::with_capacity(16)).await;
                    (n.unwrap(), buf)
                })
            };
            let mut c = &client;
            let BufResult(n, _) = c.write(b"hello".to_vec()).await;
            assert_eq!(n.unwrap(), 5);
            let (n, buf) = crate::rt::join(reader).await;
            assert_eq!(&buf.as_init()[..n], b"hello");
        });
    }

    #[test]
    fn read_into_a_slice_appends_after_existing_bytes() {
        crate::rt::LocalRuntime::new().unwrap().block_on(async {
            let (listener, bound) = Listener::bind(
                &Endpoint::parse("tcp://127.0.0.1:0").unwrap(),
                &SocketOptions::default(),
            )
            .await
            .unwrap();
            let accept = crate::rt::spawn(async move { listener.accept().await.unwrap() });
            let client = Stream::connect(&bound).await.unwrap();
            let server = crate::rt::join(accept).await;

            let mut c = &client;
            let BufResult(n, _) = c.write(b"world".to_vec()).await;
            n.unwrap();

            let mut buf = bytes::BytesMut::with_capacity(64);
            buf.extend_from_slice(b"hello ");
            let len = buf.len();
            let mut s = &server;
            let BufResult(n, slice) = s.read(buf.slice(len..)).await;
            let n = n.unwrap();
            let buf = slice.into_inner();
            assert_eq!(&buf[..], &b"hello world"[..len + n]);
        });
    }
}
