//! STREAM socket  -  raw TCP bridging without ZMTP handshake.
//!
//! STREAM sockets bridge ZeroMQ traffic to plain (non-ZMTP) TCP peers such as
//! HTTP servers, legacy protocols, and `nc`/`curl` clients.
//!
//! ## Message format
//!
//! ### Received messages (inbound from TCP peer)
//! ```text
//! Frame 0: routing-id  (8 bytes, uniquely identifies the TCP connection)
//! Frame 1: empty       (separator, matches ROUTER convention)
//! Frame 2: data        (raw bytes from the TCP stream)
//! ```
//!
//! **Connection notifications** arrive with an empty data frame:
//! - On connect: `[routing_id, "", ""]`
//! - On disconnect: `[routing_id, "", ""]`
//!
//! ### Sent messages (outbound to TCP peer)
//! ```text
//! Frame 0: routing-id  (selects the destination peer)
//! Frame 1: empty       (ignored / stripped)
//! Frame 2: data        (raw bytes written to that peer's TCP stream)
//! ```
//!
//! ## Example
//!
//! ```rust,no_run
//! use monocoque_zmtp::stream::StreamSocket;
//! use bytes::Bytes;
//!
//! # async fn example() -> std::io::Result<()> {
//! let mut srv = StreamSocket::bind("127.0.0.1:5555").await?;
//!
//! // Accept one raw TCP connection.
//! let peer_id = srv.accept_raw().await?;
//!
//! // Receive data (or a connection notification).
//! while let Some(msg) = srv.recv().await? {
//!     let routing_id = &msg[0];
//!     let data       = &msg[2];
//!     if data.is_empty() {
//!         // connection / disconnection notification
//!         continue;
//!     }
//!     // Echo back
//!     srv.send(vec![routing_id.clone(), Bytes::new(), data.clone()]).await?;
//! }
//! # Ok(())
//! # }
//! ```

use bytes::Bytes;
use flume::{Receiver, Sender};
use monocoque_core::options::SocketOptions;
use monocoque_core::rt::{OwnedReadHalf, OwnedWriteHalf, TcpListener};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, trace, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Internal types
// ─────────────────────────────────────────────────────────────────────────────

/// A routing ID is a unique 8-byte handle per TCP connection.
type RoutingId = Bytes;

/// Messages flowing from peer-reader tasks to the application.
type InboundMsg = Vec<Bytes>; // [routing_id, empty, data]

// ─────────────────────────────────────────────────────────────────────────────
// Background tasks
// ─────────────────────────────────────────────────────────────────────────────

/// Reads raw bytes from a TCP connection and forwards them to the inbound channel.
///
/// Sends a connection notification `[id, "", ""]` before the first byte, and a
/// disconnect notification `[id, "", ""]` when the connection closes.
async fn peer_reader(
    routing_id: RoutingId,
    mut reader: OwnedReadHalf,
    inbound: Sender<InboundMsg>,
) {
    use compio_buf::BufResult;
    use compio_io::AsyncRead;

    // Connection notification
    let _ = inbound
        .send_async(vec![routing_id.clone(), Bytes::new(), Bytes::new()])
        .await;

    loop {
        let buf = vec![0u8; 8192];
        let BufResult(result, buf) = reader.read(buf).await;
        match result {
            Ok(0) => {
                debug!("[STREAM] Peer {:?} disconnected (EOF)", routing_id);
                break;
            }
            Ok(n) => {
                let data = Bytes::copy_from_slice(&buf[..n]);
                trace!("[STREAM] Received {} bytes from peer {:?}", n, routing_id);
                let msg = vec![routing_id.clone(), Bytes::new(), data];
                if inbound.send_async(msg).await.is_err() {
                    break; // socket dropped
                }
            }
            Err(e) => {
                debug!("[STREAM] Peer {:?} read error: {}", routing_id, e);
                break;
            }
        }
    }

    // Disconnect notification
    let _ = inbound.try_send(vec![routing_id, Bytes::new(), Bytes::new()]);
}

/// Writes raw bytes from the per-peer send channel to the TCP connection.
async fn peer_writer(mut writer: OwnedWriteHalf, outbound: Receiver<Bytes>) {
    use compio_buf::BufResult;
    use compio_io::AsyncWriteExt;

    while let Ok(data) = outbound.recv_async().await {
        let BufResult(res, _) = writer.write_all(data.to_vec()).await;
        if res.is_err() {
            break;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// StreamSocket
// ─────────────────────────────────────────────────────────────────────────────

/// STREAM socket  -  raw TCP bridging without ZMTP handshake.
///
/// Accepts plain TCP connections and multiplexes them through a ZeroMQ-style
/// routing-ID interface.  Each accepted connection is assigned a unique 8-byte
/// routing ID; all subsequent sends and receives for that connection use the
/// same ID to route messages.
///
/// Unlike other socket types, `StreamSocket` performs **no ZMTP handshake**  -
/// it speaks plain TCP bytes, making it suitable for bridging to HTTP servers,
/// legacy services, and command-line tools such as `nc` and `curl`.
pub struct StreamSocket {
    /// TCP listener (held after `bind`, until dropped).
    listener: TcpListener,
    /// Channel from background reader tasks to the application.
    inbound_rx: Receiver<InboundMsg>,
    /// Shared sender half for reader tasks.
    inbound_tx: Sender<InboundMsg>,
    /// Per-peer outbound channels (routing_id → sender).
    peers: HashMap<RoutingId, Sender<Bytes>>,
    /// Monotonically increasing routing-ID counter.
    next_id: Arc<AtomicU64>,
    /// Socket options.
    options: SocketOptions,
}

impl StreamSocket {
    /// Bind a STREAM socket to a TCP address.
    ///
    /// The returned socket is ready to accept raw (non-ZMTP) TCP connections
    /// via [`accept_raw()`][Self::accept_raw].
    ///
    /// # Errors
    ///
    /// Returns an error if the address cannot be bound (e.g., port in use).
    pub async fn bind(addr: impl monocoque_core::rt::ToSocketAddrs) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        debug!("[STREAM] Bound to {}", listener.local_addr()?);
        let (tx, rx) = flume::unbounded();
        Ok(Self {
            listener,
            inbound_rx: rx,
            inbound_tx: tx,
            peers: HashMap::new(),
            next_id: Arc::new(AtomicU64::new(1)),
            options: SocketOptions::default(),
        })
    }

    /// Accept the next raw TCP connection and register it as a new peer.
    ///
    /// Spawns background reader and writer tasks for the connection.  Returns
    /// the routing ID assigned to this peer; the same ID is used to address
    /// messages to this peer via [`send()`][Self::send].
    ///
    /// The caller will also receive a connection notification from
    /// [`recv()`][Self::recv]: `[routing_id, "", ""]` with an empty data frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the `accept()` system call fails.
    pub async fn accept_raw(&mut self) -> io::Result<RoutingId> {
        let (stream, addr) = self.listener.accept().await?;
        crate::utils::configure_tcp_stream(&stream, &self.options, "STREAM")?;
        debug!("[STREAM] Accepted raw connection from {}", addr);

        // Generate a compact 8-byte routing ID.
        let id_u64 = self.next_id.fetch_add(1, Ordering::Relaxed);
        let routing_id = Bytes::copy_from_slice(&id_u64.to_be_bytes());

        let (read_half, write_half) = stream.into_split();

        // Per-peer outbound channel.
        let (out_tx, out_rx) = flume::unbounded::<Bytes>();
        self.peers.insert(routing_id.clone(), out_tx);

        // Spawn reader.
        let inbound = self.inbound_tx.clone();
        let rid = routing_id.clone();
        monocoque_core::rt::spawn_detached(peer_reader(rid, read_half, inbound));

        // Spawn writer.
        monocoque_core::rt::spawn_detached(peer_writer(write_half, out_rx));

        debug!("[STREAM] Peer {:?} registered", routing_id);
        Ok(routing_id)
    }

    /// Receive the next message from any connected peer.
    ///
    /// Returns `[routing_id, empty, data]`.  An empty data frame signals a
    /// connection event (connect or disconnect) for `routing_id`.
    ///
    /// Returns `Ok(None)` only if the socket's inbound channel has been
    /// closed (i.e., the `StreamSocket` itself is being dropped).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying channel has an unexpected failure.
    pub async fn recv(&mut self) -> io::Result<Option<InboundMsg>> {
        match self.inbound_rx.recv_async().await {
            Ok(msg) => {
                trace!("[STREAM] Dequeued message from peer {:?}", msg[0]);
                Ok(Some(msg))
            }
            Err(_) => Ok(None),
        }
    }

    /// Send raw bytes to a specific peer.
    ///
    /// `msg` must contain at least one frame (the routing ID).  A 3-frame
    /// layout is expected: `[routing_id, empty, data]`.  The routing ID
    /// selects the destination; remaining frames are flattened and written as
    /// raw bytes to the TCP stream.
    ///
    /// If the peer is not found (e.g., already disconnected), the message is
    /// silently dropped.
    ///
    /// # Errors
    ///
    /// Returns an error if the message has no frames or if the peer's send
    /// channel has disconnected.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        if msg.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "STREAM send requires at least a routing-id frame",
            ));
        }

        let routing_id = msg[0].clone();

        // Collect all non-routing-id, non-empty frames as raw data.
        let data: Bytes = msg
            .iter()
            .skip(1)
            .find(|f| !f.is_empty())
            .cloned()
            .unwrap_or_default();

        if data.is_empty() {
            // Sending [routing_id, ""] is a disconnect hint (libzmq semantics).
            self.disconnect(&routing_id);
            return Ok(());
        }

        match self.peers.get(&routing_id) {
            Some(tx) => {
                tx.try_send(data).map_err(|e| {
                    io::Error::other(format!("Peer {:?} send failed: {}", routing_id, e))
                })?;
                trace!("[STREAM] Queued data for peer {:?}", routing_id);
            }
            None => {
                warn!(
                    "[STREAM] Unknown routing-id {:?}, dropping message",
                    routing_id
                );
            }
        }
        Ok(())
    }

    /// Disconnect a peer explicitly, removing it from the routing table.
    ///
    /// After this call, messages addressed to `routing_id` are silently dropped.
    /// The background reader task will detect the closed write half and exit.
    pub fn disconnect(&mut self, routing_id: &Bytes) {
        if self.peers.remove(routing_id).is_some() {
            debug!("[STREAM] Peer {:?} removed from routing table", routing_id);
        }
    }

    /// Number of currently tracked (connected) peers.
    #[inline]
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// The local address this socket is bound to.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Get a reference to the socket options.
    #[inline]
    pub const fn options(&self) -> &SocketOptions {
        &self.options
    }

    /// Get a mutable reference to the socket options.
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.options
    }
}
