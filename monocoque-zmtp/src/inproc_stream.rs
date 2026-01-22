//! Inproc stream adapter for ZMQ sockets.
//!
//! Provides an AsyncRead + AsyncWrite wrapper around inproc channels,
//! allowing inproc transport to integrate seamlessly with existing socket infrastructure.

use bytes::{Buf, Bytes, BytesMut};
use compio::io::{AsyncRead, AsyncWrite};
use monocoque_core::inproc::{InprocReceiver, InprocSender};
use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Stream adapter for inproc transport.
///
/// Implements AsyncRead + AsyncWrite using flume channels for zero-copy
/// in-process communication. Messages are sent as complete frames without
/// requiring serialization.
pub struct InprocStream {
    /// Sender for outgoing messages
    tx: InprocSender,
    /// Receiver for incoming messages
    rx: InprocReceiver,
    /// Buffer for current read operation (assembled from frames)
    read_buf: BytesMut,
    /// Current read position in buffer
    read_pos: usize,
}

impl InprocStream {
    /// Create a new inproc stream from sender and receiver channels.
    pub fn new(tx: InprocSender, rx: InprocReceiver) -> Self {
        Self {
            tx,
            rx,
            read_buf: BytesMut::new(),
            read_pos: 0,
        }
    }

    /// Get a reference to the sender channel.
    pub fn sender(&self) -> &InprocSender {
        &self.tx
    }

    /// Get a reference to the receiver channel.
    pub fn receiver(&self) -> &InprocReceiver {
        &self.rx
    }
}

impl AsyncRead for InprocStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        // If we have buffered data, copy it to the output buffer
        if self.read_pos < self.read_buf.len() {
            let available = self.read_buf.len() - self.read_pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy].copy_from_slice(&self.read_buf[self.read_pos..self.read_pos + to_copy]);
            self.read_pos += to_copy;

            // If we've consumed all buffered data, clear for next message
            if self.read_pos >= self.read_buf.len() {
                self.read_buf.clear();
                self.read_pos = 0;
            }

            return Poll::Ready(Ok(to_copy));
        }

        // Need to receive a new message from the channel
        // Try non-blocking receive
        match self.rx.try_recv() {
            Ok(msg_frames) => {
                // Assemble frames into read buffer
                self.read_buf.clear();
                for frame in msg_frames {
                    self.read_buf.extend_from_slice(&frame);
                }
                self.read_pos = 0;

                // Now copy to output buffer
                let to_copy = self.read_buf.len().min(buf.len());
                if to_copy > 0 {
                    buf[..to_copy].copy_from_slice(&self.read_buf[self.read_pos..to_copy]);
                    self.read_pos += to_copy;

                    if self.read_pos >= self.read_buf.len() {
                        self.read_buf.clear();
                        self.read_pos = 0;
                    }

                    Poll::Ready(Ok(to_copy))
                } else {
                    // Empty message - shouldn't happen but handle gracefully
                    Poll::Ready(Ok(0))
                }
            }
            Err(flume::TryRecvError::Empty) => {
                // No data available, would block in async context
                // For inproc, we need to block since it's synchronous
                // Use blocking recv
                match self.rx.recv() {
                    Ok(msg_frames) => {
                        self.read_buf.clear();
                        for frame in msg_frames {
                            self.read_buf.extend_from_slice(&frame);
                        }
                        self.read_pos = 0;

                        let to_copy = self.read_buf.len().min(buf.len());
                        if to_copy > 0 {
                            buf[..to_copy].copy_from_slice(&self.read_buf[..to_copy]);
                            self.read_pos += to_copy;

                            if self.read_pos >= self.read_buf.len() {
                                self.read_buf.clear();
                                self.read_pos = 0;
                            }

                            Poll::Ready(Ok(to_copy))
                        } else {
                            Poll::Ready(Ok(0))
                        }
                    }
                    Err(_) => {
                        // Channel disconnected - EOF
                        Poll::Ready(Ok(0))
                    }
                }
            }
            Err(flume::TryRecvError::Disconnected) => {
                // Channel disconnected - EOF
                Poll::Ready(Ok(0))
            }
        }
    }
}

impl AsyncWrite for InprocStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        // For inproc, we send the entire buffer as a single frame
        // Copy data to a Bytes for zero-copy transmission
        let data = Bytes::copy_from_slice(buf);
        
        match self.tx.send(vec![data]) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "inproc receiver disconnected",
            ))),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // Gather all buffers into a single message with multiple frames
        let frames: Vec<Bytes> = bufs
            .iter()
            .map(|ioslice| Bytes::copy_from_slice(ioslice))
            .collect();

        let total_bytes: usize = bufs.iter().map(|b| b.len()).sum();

        match self.tx.send(frames) {
            Ok(()) => Poll::Ready(Ok(total_bytes)),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "inproc receiver disconnected",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Inproc channels don't need flushing
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Closing is implicit when channels are dropped
        Poll::Ready(Ok(()))
    }

    fn is_write_vectored(&self) -> bool {
        // We support vectored writes efficiently
        true
    }
}

impl std::fmt::Debug for InprocStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InprocStream")
            .field("read_buf_len", &self.read_buf.len())
            .field("read_pos", &self.read_pos)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocoque_core::inproc::{bind_inproc, connect_inproc};

    #[test]
    fn test_inproc_stream_basic() -> io::Result<()> {
        // Bind and connect
        let (tx1, rx1) = bind_inproc("inproc://test-stream")?;
        let tx2 = connect_inproc("inproc://test-stream")?;

        // Create streams
        let mut stream1 = InprocStream::new(tx1, rx1);
        let stream2 = InprocStream::new(tx2, flume::unbounded().1); // Dummy rx for this test

        // Send from stream2 to stream1
        let msg = vec![Bytes::from("hello")];
        stream2.sender().send(msg).unwrap();

        // Read on stream1 (synchronous for test)
        let mut buf = vec![0u8; 10];
        let n = std::task::block_on(async {
            use compio::io::AsyncReadExt;
            stream1.read(&mut buf).await
        })?;

        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"hello");

        Ok(())
    }
}
