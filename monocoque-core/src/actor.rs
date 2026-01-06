//! Socket Actor (Protocol-Agnostic Core)
//!
//! One actor == one TCP connection.
//!
//! Responsibilities:
//! - Own the socket (`AsyncRead` + `AsyncWrite`)
//! - Drive read + write pumps (split-pump design)
//! - Move bytes between kernel and application
//! - Emit lifecycle events (`PeerUp` / `PeerDown`)
//! - Never contain routing logic (delegated to hubs)
//! - Never contain protocol logic (delegated to protocol layer above)
//!
//! NOTE: This is a minimal primitive. ZMTP framing, session management,
//! and multipart assembly happen in a higher layer that composes this actor.

use crate::alloc::{IoArena, IoBytes, SlabMut};

use bytes::Bytes;
use flume::{Receiver, Sender};

use compio::io::{AsyncRead, AsyncWrite};

/// Messages from application to socket
#[derive(Debug)]
pub enum UserCmd {
    /// Send raw bytes
    SendBytes(Bytes),
    /// Close socket
    Close,
}

/// Events from socket to application
#[derive(Debug, Clone)]
pub enum SocketEvent {
    /// Connection established
    Connected,
    /// Received bytes from peer
    ReceivedBytes(Bytes),
    /// Connection closed
    Disconnected,
}

/// Minimal protocol-agnostic socket actor.
///
/// This is a building block. Protocol framing (ZMTP, HTTP, etc.)
/// should be layered on top by wrapping this actor.
pub struct SocketActor<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    stream: S,

    /// Channel for sending events to application
    event_tx: Sender<SocketEvent>,

    /// Channel for receiving commands from application
    cmd_rx: Receiver<UserCmd>,

    /// Allocation arena for zero-copy reads
    arena: IoArena,
}

impl<S> SocketActor<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub const fn new(
        stream: S,
        event_tx: Sender<SocketEvent>,
        cmd_rx: Receiver<UserCmd>,
        arena: IoArena,
    ) -> Self {
        Self {
            stream,
            event_tx,
            cmd_rx,
            arena,
        }
    }

    /// Run the actor event loop (split pump design).
    ///
    /// This implements the core split-pump pattern from blueprint 02:
    /// - Read pump: kernel → application (via `event_tx`)
    /// - Write pump: application → kernel (via `cmd_rx`)
    /// - No shared mutable state between pumps
    /// - Ownership-based flow control
    pub async fn run(mut self) {
        use compio::buf::BufResult;
        use compio::io::AsyncWriteExt;

        // Notify application that connection is ready
        let _ = self.event_tx.send(SocketEvent::Connected);

        // === INITIAL WRITE DRAIN ===
        // Process any queued writes (like greetings) before entering main loop
        // This prevents deadlock where both sides wait to receive before sending
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                UserCmd::SendBytes(b) => {
                    let io_buf = IoBytes::new(b);
                    let BufResult(write_res, _) = self.stream.write_all(io_buf).await;
                    if write_res.is_err() {
                        let _ = self.event_tx.send(SocketEvent::Disconnected);
                        return;
                    }
                }
                UserCmd::Close => {
                    let _ = self.event_tx.send(SocketEvent::Disconnected);
                    return;
                }
            }
        }

        // Main loop: use futures::select! to wait efficiently on multiple sources
        use futures::{select, FutureExt};

        loop {
            select! {
                // Process incoming commands (writes and close)
                cmd = self.cmd_rx.recv_async().fuse() => {
                    match cmd {
                        Ok(UserCmd::SendBytes(b)) => {
                            // Flush immediately for low latency
                            eprintln!("[SocketActor] Writing {} bytes to network", b.len());
                            let io_buf = IoBytes::new(b);
                            let BufResult(write_res, _) = self.stream.write_all(io_buf).await;
                            if write_res.is_err() {
                                eprintln!("[SocketActor] Write error, exiting");
                                let _ = self.event_tx.send(SocketEvent::Disconnected);
                                return;
                            }
                        }
                        Ok(UserCmd::Close) => {
                            let _ = self.event_tx.send(SocketEvent::Disconnected);
                            return;
                        }
                        Err(_) => {
                            // Channel closed
                            let _ = self.event_tx.send(SocketEvent::Disconnected);
                            return;
                        }
                    }
                }
                // Read from socket
                read_result = async {
                    let slab: SlabMut = self.arena.alloc_mut(8192);
                    let BufResult(res, slab) = self.stream.read(slab).await;
                    (res, slab)
                }.fuse() => {
                    match read_result {
                        (Ok(0), _) => {
                            eprintln!("[SocketActor] EOF - connection closed");
                            let _ = self.event_tx.send(SocketEvent::Disconnected);
                            break;
                        }
                        (Err(e), _) => {
                            eprintln!("[SocketActor] Read error: {e:?}");
                            let _ = self.event_tx.send(SocketEvent::Disconnected);
                            break;
                        }
                        (Ok(n), slab) => {
                            eprintln!("[SocketActor] Read {n} bytes from network");
                            let bytes = slab.freeze();
                            let _ = self.event_tx.send(SocketEvent::ReceivedBytes(bytes));
                        }
                    }
                }
            }
        }
    }
}
