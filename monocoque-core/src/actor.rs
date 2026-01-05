//! Socket Actor (Protocol-Agnostic Core)
//!
//! One actor == one TCP connection.
//!
//! Responsibilities:
//! - Own the socket (AsyncRead + AsyncWrite)
//! - Drive read + write pumps (split-pump design)
//! - Move bytes between kernel and application
//! - Emit lifecycle events (PeerUp / PeerDown)
//! - Never contain routing logic (delegated to hubs)
//! - Never contain protocol logic (delegated to protocol layer above)
//!
//! NOTE: This is a minimal primitive. ZMTP framing, session management,
//! and multipart assembly happen in a higher layer that composes this actor.

use crate::alloc::{IoArena, SlabMut};

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
    pub fn new(
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
    /// - Read pump: kernel → application (via event_tx)
    /// - Write pump: application → kernel (via cmd_rx)
    /// - No shared mutable state between pumps
    /// - Ownership-based flow control
    pub async fn run(mut self) {
        use compio::buf::BufResult;
        use compio::io::{AsyncReadExt, AsyncWriteExt};
        
        // Notify application that connection is ready
        let _ = self.event_tx.send(SocketEvent::Connected);

        let mut write_queue: Vec<Bytes> = Vec::new();

        // === INITIAL WRITE DRAIN ===
        // Process any queued writes (like greetings) before first read
        // This prevents deadlock where both sides wait to receive before sending
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                UserCmd::SendBytes(b) => write_queue.push(b),
                UserCmd::Close => {
                    let _ = self.event_tx.send(SocketEvent::Disconnected);
                    return;
                }
            }
        }
        
        // Flush initial writes immediately
        for buf in write_queue.drain(..) {
            let buf_vec = buf.to_vec();
            let BufResult(write_res, _) = (&mut self.stream).write_all(buf_vec).await;
            if write_res.is_err() {
                let _ = self.event_tx.send(SocketEvent::Disconnected);
                return;
            }
        }

        // Main loop: check writes, flush them, then try reading (with brief timeout)
        loop {
            // === WRITE PUMP (non-blocking check) ===
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                match cmd {
                    UserCmd::SendBytes(b) => write_queue.push(b),
                    UserCmd::Close => {
                        let _ = self.event_tx.send(SocketEvent::Disconnected);
                        return;
                    }
                }
            }

            // Flush pending writes
            for buf in write_queue.drain(..) {
                eprintln!("[SocketActor] Writing {} bytes to network", buf.len());
                let buf_vec = buf.to_vec();
                let BufResult(write_res, _) = (&mut self.stream).write_all(buf_vec).await;
                if write_res.is_err() {
                    eprintln!("[SocketActor] Write error, exiting");
                    let _ = self.event_tx.send(SocketEvent::Disconnected);
                    return;
                }
            }

            // === READ PUMP ===
            let slab: SlabMut = self.arena.alloc_mut(8192);
            let BufResult(read_res, slab) = (&mut self.stream).read(slab).await;
            
            match read_res {
                Ok(0) => {
                    eprintln!("[SocketActor] EOF - connection closed");
                    // EOF
                    let _ = self.event_tx.send(SocketEvent::Disconnected);
                    break;
                }
                Err(e) => {
                    eprintln!("[SocketActor] Read error: {:?}", e);
                    let _ = self.event_tx.send(SocketEvent::Disconnected);
                    break;
                }
                Ok(n) => {
                    eprintln!("[SocketActor] Read {} bytes from network", n);
                    let bytes = slab.freeze();
                    let _ = self.event_tx.send(SocketEvent::ReceivedBytes(bytes));
                }
            }
            
            // Brief yield to allow write commands to arrive
            // This prevents read() from monopolizing the loop
            compio::time::sleep(std::time::Duration::from_micros(1)).await;
        }
    }
}

