//! PUSH socket implementation.
//!
//! PUSH sockets are used in pipeline patterns for distributing tasks.

use compio::net::TcpStream;
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::PushSocket as InternalPush;
use std::io;

/// PUSH socket for distributing tasks in a pipeline.
///
/// PUSH sockets send messages in a round-robin fashion to connected PULL sockets.
pub struct PushSocket<S = TcpStream>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    inner: InternalPush<S>,
}

impl PushSocket<TcpStream> {
    /// Create a PUSH socket from a TCP stream.
    pub async fn from_tcp(stream: TcpStream) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::from_tcp(stream).await?,
        })
    }

    /// Create a PUSH socket from a TCP stream with custom options.
    pub async fn from_tcp_with_options(
        stream: TcpStream,
        options: SocketOptions,
    ) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::from_tcp_with_options(stream, options).await?,
        })
    }
}

impl<S> PushSocket<S>
where
    S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin,
{
    /// Create a PUSH socket from any stream.
    pub async fn new(stream: S) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::new(stream).await?,
        })
    }

    /// Create a PUSH socket from any stream with custom options.
    pub async fn with_options(stream: S, options: SocketOptions) -> io::Result<Self> {
        Ok(Self {
            inner: InternalPush::with_options(stream, options).await?,
        })
    }

    /// Send a message.
    pub async fn send(&mut self, msg: Vec<bytes::Bytes>) -> io::Result<()> {
        self.inner.send(msg).await
    }
}
