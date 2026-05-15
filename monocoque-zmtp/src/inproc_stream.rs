//! Inproc stream adapter for ZMQ sockets.
//!
//! Provides an AsyncRead + AsyncWrite wrapper around inproc channels,
//! allowing inproc transport to integrate seamlessly with existing socket infrastructure.

use bytes::{Bytes, BytesMut};
use compio::buf::{BufResult, IoBuf, IoBufMut};
use compio::io::{AsyncRead, AsyncWrite};
use monocoque_core::inproc::{InprocReceiver, InprocSender};
use std::io;

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
    pub const fn sender(&self) -> &InprocSender {
        &self.tx
    }

    /// Get a reference to the receiver channel.
    pub const fn receiver(&self) -> &InprocReceiver {
        &self.rx
    }
}

impl AsyncRead for InprocStream {
    async fn read<B: IoBufMut>(&mut self, mut buf: B) -> BufResult<usize, B> {
        match self.rx.recv_async().await {
            Ok(msg_frames) => {
                // Assemble frames and copy to buffer
                let mut total = 0;
                let buf_capacity = buf.buf_capacity();

                for frame in msg_frames {
                    let to_copy = frame.len().min(buf_capacity - total);
                    if to_copy == 0 {
                        break;
                    }
                    // Copy data using safe slice API
                    let dest_slice = unsafe {
                        std::slice::from_raw_parts_mut(
                            buf.as_slice().as_ptr().cast_mut().add(total),
                            to_copy,
                        )
                    };
                    dest_slice.copy_from_slice(&frame[..to_copy]);
                    total += to_copy;
                }

                unsafe {
                    buf.set_buf_init(total);
                }
                BufResult(Ok(total), buf)
            }
            Err(_) => {
                // Channel disconnected - EOF
                BufResult(Ok(0), buf)
            }
        }
    }
}

impl AsyncWrite for InprocStream {
    async fn write<B: IoBuf>(&mut self, buf: B) -> BufResult<usize, B> {
        // For inproc, we send the entire buffer as a single frame
        let len = buf.buf_len();
        let data = Bytes::copy_from_slice(buf.as_slice());

        match self.tx.send(vec![data]) {
            Ok(()) => BufResult(Ok(len), buf),
            Err(_) => BufResult(
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "inproc receiver disconnected",
                )),
                buf,
            ),
        }
    }

    async fn flush(&mut self) -> io::Result<()> {
        // Inproc channels don't need flushing
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        // Closing is implicit when channels are dropped
        Ok(())
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
        use compio::buf::BufResult;
        use compio::io::AsyncRead;

        let endpoint = "inproc://test-stream-basic";
        let (tx1, rx1) = bind_inproc(endpoint)?;
        let tx2 = connect_inproc(endpoint)?;

        let mut stream1 = InprocStream::new(tx1, rx1);

        // Send data into stream1's receiver before reading (channel buffers it)
        tx2.send(vec![Bytes::from_static(b"hello")]).unwrap();

        let rt = compio::runtime::Runtime::new()?;
        let (n, buf) = rt.block_on(async {
            let buf = vec![0u8; 10];
            let BufResult(result, buf) = AsyncRead::read(&mut stream1, buf).await;
            result.map(|n| (n, buf))
        })?;

        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"hello");

        // Cleanup global registry
        monocoque_core::inproc::unbind_inproc(endpoint)?;
        Ok(())
    }
}
