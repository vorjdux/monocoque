//! Inproc stream adapter for ZMQ sockets.
//!
//! Provides an AsyncRead + AsyncWrite wrapper around inproc channels,
//! allowing inproc transport to integrate seamlessly with existing socket infrastructure.

use bytes::{Bytes, BytesMut};
use compio_buf::{BufResult, IoBuf, IoBufMut};
use compio_io::{AsyncRead, AsyncWrite};
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
        let buf_cap = buf.buf_capacity();
        let buf_ptr = buf.as_buf_mut_ptr();
        let mut total = 0usize;

        if self.read_pos < self.read_buf.len() {
            let available = &self.read_buf[self.read_pos..];
            let to_copy = available.len().min(buf_cap);
            unsafe {
                std::ptr::copy_nonoverlapping(available.as_ptr(), buf_ptr, to_copy);
                buf.set_buf_init(to_copy);
            }
            self.read_pos += to_copy;
            if self.read_pos == self.read_buf.len() {
                self.read_buf.clear();
                self.read_pos = 0;
            }
            return BufResult(Ok(to_copy), buf);
        }

        match self.rx.recv_async().await {
            Ok(msg_frames) => {
                for frame in msg_frames {
                    let remaining = buf_cap - total;
                    let to_copy = frame.len().min(remaining);
                    if to_copy == 0 {
                        self.read_buf.extend_from_slice(&frame);
                        break;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(frame.as_ptr(), buf_ptr.add(total), to_copy);
                    }
                    total += to_copy;
                    if to_copy < frame.len() {
                        self.read_buf.extend_from_slice(&frame[to_copy..]);
                        break;
                    }
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
        use compio_buf::BufResult;
        use compio_io::AsyncRead;

        let endpoint = "inproc://test-stream-basic";
        let (tx1, rx1) = bind_inproc(endpoint)?;
        let tx2 = connect_inproc(endpoint)?;

        let mut stream1 = InprocStream::new(tx1, rx1);

        // Send data into stream1's receiver before reading (channel buffers it)
        tx2.send(vec![Bytes::from_static(b"hello")]).unwrap();

        let rt = monocoque_core::rt::LocalRuntime::new()?;
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

    #[test]
    fn test_inproc_stream_preserves_partial_read_remainder() -> io::Result<()> {
        use compio::buf::BufResult;
        use compio::io::AsyncRead;

        let endpoint = "inproc://test-stream-partial-read";
        let (tx1, rx1) = bind_inproc(endpoint)?;
        let tx2 = connect_inproc(endpoint)?;

        let mut stream1 = InprocStream::new(tx1, rx1);
        tx2.send(vec![Bytes::from_static(b"hello")]).unwrap();
        tx2.send(vec![Bytes::from_static(b"next")]).unwrap();

        let rt = compio::runtime::Runtime::new()?;
        let ((first_n, first_buf), (second_n, second_buf)) = rt.block_on(async {
            let first_buf = vec![0u8; 2];
            let BufResult(first_result, first_buf) = AsyncRead::read(&mut stream1, first_buf).await;
            let first = first_result.map(|n| (n, first_buf))?;

            let second_buf = vec![0u8; 3];
            let BufResult(second_result, second_buf) =
                AsyncRead::read(&mut stream1, second_buf).await;
            let second = second_result.map(|n| (n, second_buf))?;

            io::Result::Ok((first, second))
        })?;

        assert_eq!(&first_buf[..first_n], b"he");
        assert_eq!(&second_buf[..second_n], b"llo");

        monocoque_core::inproc::unbind_inproc(endpoint)?;
        Ok(())
    }
}
