//! PULL socket implementation.
//!
//! PULL sockets are used in pipeline patterns for receiving tasks.

use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::PullSocket as InternalPull;
use std::io;

/// PULL socket for receiving tasks in a pipeline.
///
/// PULL sockets receive messages from connected PUSH sockets.
pub struct PullSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalPull<S>,
}

impl PullSocket<TcpStream> {
    /// Create a PULL socket from a TCP stream.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::from_tcp(stream).await?,
        })
    }

    /// Create a PULL socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::from_tcp_with_options(stream, options).await?,
        })
    }
}

impl<S> PullSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Create a PULL socket from any stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::new(stream).await?,
        })
    }

    /// Create a PULL socket from any stream with custom options.
    pub async fn with_options(stream: S, options: SocketOptions) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPull::with_options(stream, options).await?,
        })
    }

    /// Receive a message.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        self.inner.recv().await
    }
}
