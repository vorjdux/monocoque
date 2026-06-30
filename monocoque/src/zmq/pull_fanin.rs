//! Fan-in PULL socket: one sink collecting results from many PUSH workers.
//!
//! A plain [`PullSocket`](crate::zmq::PullSocket) owns a single connection, so
//! binding it accepts exactly one worker. `PullFanIn` binds once, accepts a
//! pool of PUSH workers, and merges their messages into one fair-queued stream.
//!
//! ```text
//! [Worker PUSH 0] --\
//! [Worker PUSH 1] ---> [Sink / PullFanIn] --recv--> messages
//! [Worker PUSH N] --/
//! ```
//!
//! Each accepted connection gets its own reader task that drives that socket's
//! `recv` to completion and forwards messages into a shared channel. Giving each
//! connection a dedicated task keeps reads cancellation-safe (no half-read
//! connection is ever abandoned) and the channel order is the fair-queue order
//! the messages actually arrived in.
//!
//! The handoff is batched. A reader drains every message decoded from one kernel
//! read with [`PullSocket::recv_batch`] and forwards the whole batch as a single
//! channel item; the sink pops from a local buffer that it only refills from the
//! channel when empty. That amortizes the cross-task channel hop and the
//! per-message `.await` over a whole batch, which is what the sink would
//! otherwise pay per message. The batch size follows what the kernel delivers per
//! read, so coalescing senders produce large batches and eager senders fall back
//! to small ones without a separate code path.
//!
//! The readers and the consumer share one runtime on purpose. A per-connection
//! thread (one runtime each) was measured and is a net loss for the small-message
//! merge rate this type is built for: at ~10M msg/s the decode is cheap and the
//! cost is dominated by cross-core traffic (cache-line bouncing plus the atomic
//! `Bytes` refcount drop landing on a different core than the one that created
//! it). Keeping it on one runtime keeps that traffic on one core. Threads only
//! pay off for large, decode-heavy messages, where the link bandwidth is the real
//! limit anyway.

use flume::{Receiver, Sender};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{JoinHandle, spawn};
use monocoque_core::rt::{TcpListener, TcpStream};
use std::collections::VecDeque;
use std::io;

use super::PullSocket;

/// One channel item is a whole batch of messages drained from a single kernel
/// read. This bounds how many *batches* may queue before reader tasks wait for
/// the sink to catch up, capping memory without throttling a sink that keeps up.
const CHANNEL_CAPACITY: usize = 1024;

/// A batch of complete multipart messages handed across the merge channel in one
/// hop.
type Batch = Vec<Vec<bytes::Bytes>>;

/// A PULL endpoint that merges messages from a pool of PUSH workers.
///
/// Create one with [`bind`](Self::bind), or bind a listener yourself and call
/// [`accept_workers`](Self::accept_workers) when you need the bound port before
/// the workers connect (the bench peer does this to print its port).
pub struct PullFanIn {
    rx: Receiver<Batch>,
    /// Messages from the last channel batch not yet handed out, drained one at a
    /// time by `recv`/`try_recv` before the next channel hop.
    buf: VecDeque<Vec<bytes::Bytes>>,
    // Reader tasks are kept alive here. Dropping the handles cancels them, which
    // is exactly what we want when the sink goes away.
    _readers: Vec<JoinHandle<()>>,
}

impl PullFanIn {
    /// Bind to `addr`, accept `n_workers` PUSH connections, and return the
    /// listener alongside the ready fan-in socket.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PullFanIn;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let (_listener, mut sink) = PullFanIn::bind("127.0.0.1:5558", 4).await?;
    /// while let Ok(Some(result)) = sink.recv().await {
    ///     // process result
    ///     drop(result);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(
        addr: impl monocoque_core::rt::ToSocketAddrs,
        n_workers: usize,
    ) -> io::Result<(TcpListener, Self)> {
        Self::bind_with_options(addr, n_workers, SocketOptions::default()).await
    }

    /// Like [`bind`](Self::bind) but applies `options` to every worker connection.
    pub async fn bind_with_options(
        addr: impl monocoque_core::rt::ToSocketAddrs,
        n_workers: usize,
        options: SocketOptions,
    ) -> io::Result<(TcpListener, Self)> {
        let listener = TcpListener::bind(addr).await?;
        let fanin = Self::accept_workers(&listener, n_workers, options).await?;
        Ok((listener, fanin))
    }

    /// Accept `n_workers` PUSH connections on an already-bound listener.
    ///
    /// Useful when the bound address must be read (and announced) before the
    /// workers are allowed to connect.
    pub async fn accept_workers(
        listener: &TcpListener,
        n_workers: usize,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let (tx, rx) = flume::bounded(CHANNEL_CAPACITY);
        let mut readers = Vec::with_capacity(n_workers);
        for _ in 0..n_workers {
            let (stream, _) = listener.accept().await?;
            let pull = PullSocket::from_tcp_with_options(stream, options.clone()).await?;
            readers.push(spawn(read_into_channel(pull, tx.clone())));
        }
        // Drop our own sender so the channel closes once every reader is done.
        drop(tx);
        Ok(Self {
            rx,
            buf: VecDeque::new(),
            _readers: readers,
        })
    }

    /// Receive the next message from any worker.
    ///
    /// Pops from the local buffer first and only waits on the channel when the
    /// buffer is empty. Returns `Ok(None)` once every worker has disconnected and
    /// both the buffer and channel have drained, mirroring `PullSocket::recv` on a
    /// closed connection.
    pub async fn recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        if let Some(msg) = self.buf.pop_front() {
            return Ok(Some(msg));
        }
        // A channel error means every sender (reader task) has dropped, i.e. all
        // workers disconnected; surface that as a clean end of stream.
        match self.rx.recv_async().await {
            Ok(batch) => {
                self.buf.extend(batch);
                Ok(self.buf.pop_front())
            }
            Err(_) => Ok(None),
        }
    }

    /// Try to receive a message without waiting.
    ///
    /// Returns `Ok(None)` when nothing is buffered or queued, even if workers are
    /// still connected. Use it to drain the sink after a `recv`.
    pub fn try_recv(&mut self) -> io::Result<Option<Vec<bytes::Bytes>>> {
        if let Some(msg) = self.buf.pop_front() {
            return Ok(Some(msg));
        }
        // Both "empty" and "disconnected" map to "nothing to hand back now".
        match self.rx.try_recv() {
            Ok(batch) => {
                self.buf.extend(batch);
                Ok(self.buf.pop_front())
            }
            Err(_) => Ok(None),
        }
    }

    /// Receive a burst of merged messages with a single `.await`.
    ///
    /// Returns the locally buffered messages plus every batch already queued from
    /// the worker readers, folded into one `Vec`. Blocks for at least one message
    /// when nothing is ready yet. Returning a burst from one `.await` amortizes
    /// the per-await cost for throughput-bound sinks; it is the fan-in counterpart
    /// to [`PullSocket::recv_batch`](crate::zmq::PullSocket::recv_batch).
    ///
    /// Returns `Ok(None)` once every worker has disconnected and nothing remains.
    pub async fn recv_batch(&mut self) -> io::Result<Option<Vec<Vec<bytes::Bytes>>>> {
        let mut out: Vec<Vec<bytes::Bytes>> = self.buf.drain(..).collect();
        if out.is_empty() {
            match self.rx.recv_async().await {
                Ok(batch) => out = batch,
                Err(_) => return Ok(None),
            }
        }
        // Fold in any further batches already waiting, without blocking.
        while let Ok(batch) = self.rx.try_recv() {
            out.extend(batch);
        }
        Ok(Some(out))
    }
}

/// Drive one worker connection, forwarding each kernel-read batch into the merge
/// channel as a single item.
///
/// Exits when the connection closes, the worker errors, or the sink drops the
/// receiver (so `send_async` fails). Any of those just means this worker is done.
async fn read_into_channel(mut pull: PullSocket<TcpStream>, tx: Sender<Batch>) {
    loop {
        match pull.recv_batch().await {
            Ok(Some(batch)) => {
                if tx.send_async(batch).await.is_err() {
                    return;
                }
            }
            // Connection closed or errored: this worker has nothing more to give.
            _ => return,
        }
    }
}
