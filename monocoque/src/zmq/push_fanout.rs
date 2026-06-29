//! Fan-out PUSH socket: one ventilator spreading work across many PULL workers.
//!
//! A plain [`PushSocket`](crate::zmq::PushSocket) owns a single connection, so
//! binding it accepts exactly one worker. `PushFanOut` binds once, accepts a
//! whole pool of PULL workers, and hands each `send` to the next worker in turn.
//! That is the ZMQ PUSH load-balancing rule: every message goes to exactly one
//! worker, and consecutive messages rotate through the pool.
//!
//! ```text
//! [Ventilator / PushFanOut] --round-robin--> [Worker PULL 0]
//!                                        \--> [Worker PULL 1]
//!                                         \-> [Worker PULL N]
//! ```
//!
//! Workers connect with an ordinary `PullSocket::connect`, so the worker side
//! needs no special type.

use compio::net::{TcpListener, TcpStream};
use monocoque_core::options::SocketOptions;
use std::io;

use super::PushSocket;

/// A PUSH endpoint that distributes messages across a pool of PULL workers.
///
/// Create one with [`bind`](Self::bind), or bind a listener yourself and call
/// [`accept_workers`](Self::accept_workers) when you need the bound port before
/// the workers connect (the bench peer does this to print its port).
pub struct PushFanOut {
    workers: Vec<PushSocket<TcpStream>>,
    next: usize,
}

impl PushFanOut {
    /// Bind to `addr`, accept `n_workers` PULL connections, and return the
    /// listener alongside the ready fan-out socket.
    ///
    /// The listener is returned so the caller can keep accepting late workers
    /// with [`accept`](Self::accept) if it wants to grow the pool.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use monocoque::zmq::PushFanOut;
    /// use bytes::Bytes;
    ///
    /// # async fn example() -> std::io::Result<()> {
    /// let (_listener, mut vent) = PushFanOut::bind("127.0.0.1:5557", 4).await?;
    /// for i in 0..100 {
    ///     vent.send(vec![Bytes::from(format!("task-{i}"))]).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn bind(
        addr: impl compio::net::ToSocketAddrsAsync,
        n_workers: usize,
    ) -> io::Result<(TcpListener, Self)> {
        Self::bind_with_options(addr, n_workers, SocketOptions::default()).await
    }

    /// Like [`bind`](Self::bind) but applies `options` to every worker connection.
    pub async fn bind_with_options(
        addr: impl compio::net::ToSocketAddrsAsync,
        n_workers: usize,
        options: SocketOptions,
    ) -> io::Result<(TcpListener, Self)> {
        let listener = TcpListener::bind(addr).await?;
        let fanout = Self::accept_workers(&listener, n_workers, options).await?;
        Ok((listener, fanout))
    }

    /// Accept `n_workers` PULL connections on an already-bound listener.
    ///
    /// Useful when the bound address must be read (and announced) before the
    /// workers are allowed to connect.
    pub async fn accept_workers(
        listener: &TcpListener,
        n_workers: usize,
        options: SocketOptions,
    ) -> io::Result<Self> {
        let mut workers = Vec::with_capacity(n_workers);
        for _ in 0..n_workers {
            let (stream, _) = listener.accept().await?;
            workers.push(PushSocket::from_tcp_with_options(stream, options.clone()).await?);
        }
        Ok(Self { workers, next: 0 })
    }

    /// Accept one more worker on `listener` and add it to the pool.
    pub async fn accept(&mut self, listener: &TcpListener) -> io::Result<()> {
        let (stream, _) = listener.accept().await?;
        self.workers.push(PushSocket::from_tcp(stream).await?);
        Ok(())
    }

    /// Number of workers currently in the pool.
    #[inline]
    pub fn len(&self) -> usize {
        self.workers.len()
    }

    /// True when no workers remain.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Send one message to the next worker in round-robin order.
    ///
    /// Workers already known to be disconnected are skipped (and dropped from the
    /// pool) before the message is handed over, so a send that fails mid-flight
    /// only fails the current call; the failed worker is routed around from the
    /// next send onward. The call only errors when no live worker remains.
    ///
    /// The message bodies stay zero-copy (`Bytes` are refcounted) and the healthy
    /// path moves `msg` straight into the chosen worker, so it adds no per-message
    /// allocation over a plain `PushSocket::send`.
    pub async fn send(&mut self, msg: Vec<bytes::Bytes>) -> io::Result<()> {
        // Advance to the next worker that still looks connected, dropping any
        // known-dead ones on the way. This needs no copy of `msg`, so the common
        // all-healthy case moves the message in without an extra allocation.
        while !self.workers.is_empty() {
            let idx = self.next % self.workers.len();
            if !self.workers[idx].is_connected() {
                // Drop the dead worker; `idx` now indexes whatever shifted into
                // its place, so leave `next` pointing there.
                self.workers.remove(idx);
                self.next = idx;
                continue;
            }

            return match self.workers[idx].send(msg).await {
                Ok(()) => {
                    self.next = idx + 1;
                    Ok(())
                }
                Err(e) => {
                    // The send failed: the worker is disconnected, or it was
                    // poisoned by a cancelled write (a poisoned socket keeps its
                    // stream, so `is_connected()` alone would not catch it).
                    // Either way it is unusable, so drop it and route around it
                    // from the next send.
                    self.workers.remove(idx);
                    self.next = idx;
                    Err(e)
                }
            };
        }

        Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "PushFanOut has no live workers",
        ))
    }

    /// Flush every worker's write-coalescing buffer.
    ///
    /// Call this after the last `send` in a burst when the workers were created
    /// with write coalescing enabled.
    pub async fn flush(&mut self) -> io::Result<()> {
        for worker in &mut self.workers {
            worker.flush().await?;
        }
        Ok(())
    }
}
