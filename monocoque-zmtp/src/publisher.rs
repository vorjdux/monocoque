/// High-performance PUB socket implementation with worker pool.
///
/// This module provides a scalable PUB socket that can broadcast to multiple
/// subscribers with minimal overhead using:
/// - **Worker pool architecture**: Multiple threads handle subscribers in parallel
/// - **io_uring per worker**: Each worker thread runs its own compio runtime with io_uring
/// - **Zero-copy message broadcasting**: Shared Arc<Bytes> across all workers
/// - **Lock-free subscription updates**: Per-subscriber subscription state with RwLock
///
/// ## Architecture:
///
/// ```text
/// Main Thread                    Worker Threads (e.g., 4 workers)
/// ┌─────────────┐               ┌──────────────────────────────┐
/// │  PubSocket  │               │ Worker 1 (compio runtime)    │
/// │             │───accept───────▶│ - Subscriber 1, 5, 9, ...   │
/// │  send()     │               │ - Read subscriptions          │
/// │             │───broadcast───▶│ - Write messages             │
/// └─────────────┘               └──────────────────────────────┘
///       │                        ┌──────────────────────────────┐
///       │                        │ Worker 2 (compio runtime)    │
///       └────────────────────────▶│ - Subscriber 2, 6, 10, ...  │
///                                │ - Read subscriptions          │
///                                └──────────────────────────────┘
///                                         ... (Workers 3, 4)
/// ```
///
/// ## Benefits:
/// - **Scalability**: Handle 1000+ subscribers with 4-8 workers
/// - **No blocking**: Subscription reads don't block broadcasts
/// - **Load balancing**: Round-robin subscriber distribution
/// - **Fault isolation**: One subscriber's errors don't affect others
///
/// ## Performance characteristics:
/// - O(1) subscriber add (via channel to worker)
/// - O(k/w) broadcast per worker where k=subscribers, w=workers (parallel)
/// - O(n) topic matching where n=topic prefix length
/// - Zero-copy via Arc<Bytes> for message data
use bytes::Bytes;
use compio::net::{OwnedReadHalf, OwnedWriteHalf, TcpListener, TcpStream};
use flume::{Receiver, Sender};
use monocoque_core::subscription::SubscriptionEvent;

use crate::handshake::perform_handshake_with_options;
use crate::session::SocketType;
use monocoque_core::options::SocketOptions;
use monocoque_core::poison::PoisonGuard;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use tracing::{debug, error, trace, warn};

/// Unique identifier for each subscriber connection
type SubscriberId = u64;

/// Shared subscription state (updated by subscription reader, read by broadcast sender)
type SubscriptionState = Arc<RwLock<Vec<Bytes>>>;

/// Commands sent from main thread to worker threads
enum WorkerCommand {
    /// Add a new subscriber to this worker
    AddSubscriber {
        id: SubscriberId,
        stream: TcpStream,
        subscriptions: SubscriptionState,
    },
    /// Broadcast a message to all subscribers in this worker
    Broadcast { message: Arc<Vec<Bytes>> },
    /// Shutdown the worker
    Shutdown,
}

/// Per-subscriber state managed by worker
struct WorkerSubscriber {
    id: SubscriberId,
    stream: OwnedWriteHalf<TcpStream>,
    subscriptions: SubscriptionState,
}

impl WorkerSubscriber {
    /// Check if message matches subscriber's subscriptions
    fn matches(&self, msg: &[Bytes]) -> bool {
        let subs = self.subscriptions.read();

        // Empty subscriptions = subscribe to all
        if subs.is_empty() {
            return true;
        }

        // Check first frame against subscription prefixes
        if let Some(first_frame) = msg.first() {
            for sub in subs.iter() {
                if sub.is_empty()
                    || (first_frame.len() >= sub.len() && first_frame[..sub.len()] == sub[..])
                {
                    return true;
                }
            }
        }
        false
    }
}

/// Background task that reads subscription messages from a subscriber.
///
/// Subscription messages are ZMTP frames carrying `\x01prefix` (subscribe) or
/// `\x00prefix` (unsubscribe) payloads.  ZMTP framing provides a length header
/// so consecutive messages can always be split correctly even if they arrive in
/// the same TCP segment.
async fn subscription_reader(
    id: SubscriberId,
    mut reader: OwnedReadHalf<TcpStream>,
    subscriptions: SubscriptionState,
) {
    use compio::buf::BufResult;
    use compio::io::AsyncRead;
    use monocoque_core::buffer::SegmentedBuffer;

    trace!("[PUB] Subscription reader started for subscriber {}", id);

    let mut recv_buf = SegmentedBuffer::new();
    let mut decoder = crate::codec::ZmtpDecoder::new();

    loop {
        // Read a chunk from the subscriber.
        let buf = vec![0u8; 256];
        let BufResult(result, buf) = reader.read(buf).await;

        match result {
            Ok(0) => {
                debug!("[PUB] Subscriber {} disconnected (subscription reader)", id);
                break;
            }
            Ok(n) => {
                recv_buf.push(Bytes::from(buf[..n].to_vec()));

                // Drain all complete ZMTP frames from the accumulated buffer.
                loop {
                    match decoder.decode(&mut recv_buf) {
                        Ok(Some(frame)) => {
                            if let Some(event) =
                                SubscriptionEvent::from_message(&frame.payload)
                            {
                                let mut subs = subscriptions.write();
                                match event {
                                    SubscriptionEvent::Subscribe(prefix) => {
                                        if !subs.contains(&prefix) {
                                            trace!(
                                                "[PUB] Subscriber {} subscribed to {:?}",
                                                id, prefix
                                            );
                                            subs.push(prefix);
                                        }
                                    }
                                    SubscriptionEvent::Unsubscribe(prefix) => {
                                        trace!(
                                            "[PUB] Subscriber {} unsubscribed from {:?}",
                                            id, prefix
                                        );
                                        subs.retain(|s| s != &prefix);
                                    }
                                }
                            }
                        }
                        Ok(None) => break, // need more data
                        Err(e) => {
                            debug!(
                                "[PUB] Subscription reader for subscriber {} decode error: {}",
                                id, e
                            );
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                debug!(
                    "[PUB] Subscription reader for subscriber {} error: {}",
                    id, e
                );
                break;
            }
        }
    }

    trace!("[PUB] Subscription reader exiting for subscriber {}", id);
}

/// Worker thread that handles multiple subscribers
fn worker_thread(worker_id: usize, rx: Receiver<WorkerCommand>) {
    debug!("[Worker {}] Starting", worker_id);

    let rt = match compio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("[Worker {}] Failed to create runtime: {}", worker_id, e);
            return;
        }
    };

    rt.block_on(async move {
        let mut subscribers: HashMap<SubscriberId, WorkerSubscriber> = HashMap::new();

        loop {
            match rx.recv_async().await {
                Ok(WorkerCommand::AddSubscriber {
                    id,
                    stream,
                    subscriptions,
                }) => {
                    debug!("[Worker {}] Adding subscriber {}", worker_id, id);

                    // Split stream into read half (subscriptions) and write half (broadcasts)
                    let (read_half, write_half) = stream.into_split();

                    // Spawn background task to read subscription messages.
                    // Runs concurrently within this worker's compio runtime.
                    let sub_state = Arc::clone(&subscriptions);
                    compio::runtime::spawn(subscription_reader(id, read_half, sub_state)).detach();

                    subscribers.insert(
                        id,
                        WorkerSubscriber {
                            id,
                            stream: write_half,
                            subscriptions,
                        },
                    );
                }
                Ok(WorkerCommand::Broadcast { message }) => {
                    trace!(
                        "[Worker {}] Broadcasting to {} subscribers",
                        worker_id,
                        subscribers.len()
                    );

                    // Encode once, broadcast N times via O(1) Bytes::clone()
                    let mut wire_buf = bytes::BytesMut::new();
                    crate::codec::encode_multipart(&message, &mut wire_buf);
                    let wire = wire_buf.freeze();

                    let mut dead_subs = Vec::new();
                    for sub in subscribers.values_mut() {
                        // Skip non-matching subscribers
                        if !sub.matches(&message) {
                            continue;
                        }

                        // Send with 5-second timeout for fault isolation
                        let send_result = compio::time::timeout(
                            std::time::Duration::from_secs(5),
                            send_encoded_to_stream(&mut sub.stream, wire.clone()),
                        )
                        .await;

                        match send_result {
                            Ok(Ok(())) => {
                                trace!("[Worker {}] Sent to subscriber {}", worker_id, sub.id);
                            }
                            Ok(Err(e)) => {
                                debug!(
                                    "[Worker {}] Subscriber {} send error: {}",
                                    worker_id, sub.id, e
                                );
                                dead_subs.push(sub.id);
                            }
                            Err(_) => {
                                warn!("[Worker {}] Subscriber {} timed out", worker_id, sub.id);
                                dead_subs.push(sub.id);
                            }
                        }
                    }

                    // Clean up failed subscribers
                    for id in dead_subs {
                        subscribers.remove(&id);
                        debug!("[Worker {}] Removed dead subscriber {}", worker_id, id);
                    }
                }
                Ok(WorkerCommand::Shutdown) => {
                    debug!("[Worker {}] Shutting down", worker_id);
                    break;
                }
                Err(_) => {
                    debug!("[Worker {}] Channel closed, exiting", worker_id);
                    break;
                }
            }
        }
    });

    debug!("[Worker {}] Stopped", worker_id);
}

/// Write a pre-encoded ZMTP wire frame to a subscriber stream.
///
/// Takes ownership of `data` (required by compio's io_uring completion model)
/// and discards it after the write completes. Callers broadcast via
/// `Bytes::clone()`, which is an O(1) refcount bump with no data copy.
async fn send_encoded_to_stream(
    stream: &mut OwnedWriteHalf<TcpStream>,
    data: Bytes,
) -> io::Result<()> {
    use compio::buf::BufResult;
    use compio::io::AsyncWriteExt;
    let BufResult(res, _) = stream.write_all(data).await;
    res
}

/// PUB socket with worker pool for multi-subscriber broadcasting
///
/// Uses multiple worker threads to handle subscribers in parallel.
/// Each worker runs its own compio runtime with io_uring.
pub struct PubSocket {
    /// Worker thread channels (bounded by send_hwm for backpressure)
    workers: Vec<Sender<WorkerCommand>>,
    /// Next subscriber ID
    next_id: SubscriberId,
    /// Next worker to assign (round-robin)
    next_worker: usize,
    /// Socket options
    options: SocketOptions,
    /// Subscriber count
    subscriber_count: usize,
    /// Connection health flag (true if send was cancelled mid-operation)
    is_poisoned: bool,
    /// Messages dropped due to full worker channels (HWM enforcement)
    drop_count: Arc<AtomicU64>,
}

impl PubSocket {
    /// Create a new PUB socket with default worker count (number of CPU cores).
    pub fn new() -> Self {
        Self::with_workers(num_cpus::get().max(2))
    }

    /// Create with a specific number of worker threads and default options.
    pub fn with_workers(worker_count: usize) -> Self {
        Self::with_workers_opts(worker_count, SocketOptions::default())
    }

    /// Create with a specific number of worker threads and custom socket options.
    ///
    /// Worker channels are bounded by `options.send_hwm`. When a worker's channel
    /// is full (the worker is slow/blocked), broadcast messages for that worker are
    /// silently dropped and counted in `drop_count()`.
    pub fn with_workers_opts(worker_count: usize, options: SocketOptions) -> Self {
        let hwm = options.send_hwm;
        debug!(
            "[PUB] Starting {} worker threads (channel HWM={})",
            worker_count, hwm
        );

        let mut workers = Vec::with_capacity(worker_count);

        for i in 0..worker_count {
            let (tx, rx) = flume::bounded(hwm);
            thread::Builder::new()
                .name(format!("pub-worker-{}", i))
                .spawn(move || worker_thread(i, rx))
                .expect("Failed to spawn worker thread");
            workers.push(tx);
        }

        Self {
            workers,
            next_id: 1,
            next_worker: 0,
            options,
            subscriber_count: 0,
            is_poisoned: false,
            drop_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Accept a new subscriber connection
    ///
    /// Performs ZMTP handshake and assigns subscriber to a worker thread (round-robin).
    /// A background subscription reader task is spawned in the worker to handle
    /// subscribe/unsubscribe messages from this subscriber.
    pub async fn accept_subscriber(&mut self, listener: &TcpListener) -> io::Result<SubscriberId> {
        let (stream, addr) = listener.accept().await?;

        crate::utils::configure_tcp_stream(&stream, &self.options, "PUB")?;
        debug!("[PUB] Accepted connection from {}", addr);

        let mut stream = stream;
        let handshake_result = perform_handshake_with_options(
            &mut stream,
            SocketType::Pub,
            None,
            Some(self.options.handshake_timeout),
            &self.options,
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;

        debug!(peer_socket_type = ?handshake_result.peer_socket_type, "[PUB] Handshake complete");

        let id = self.next_id;
        self.next_id += 1;

        // Create subscription state for this subscriber (starts empty = match all)
        let subscriptions = Arc::new(RwLock::new(Vec::new()));

        // Assign to next worker (round-robin)
        let worker_idx = self.next_worker;
        self.next_worker = (self.next_worker + 1) % self.workers.len();

        debug!("[PUB] Assigning subscriber {} to worker {}", id, worker_idx);

        // Send stream + subscriptions to worker. The worker will split the stream
        // into read (subscription reader task) and write (broadcast) halves.
        self.workers[worker_idx]
            .send_async(WorkerCommand::AddSubscriber {
                id,
                stream,
                subscriptions,
            })
            .await
            .map_err(|e| io::Error::other(format!("Failed to send to worker: {}", e)))?;

        self.subscriber_count += 1;
        debug!(
            "[PUB] Subscriber {} accepted and subscription reader started (total: {})",
            id, self.subscriber_count
        );

        Ok(id)
    }

    /// Remove a subscriber (handled automatically by worker drop)
    pub fn remove_subscriber(&mut self, _id: SubscriberId) {
        // No-op: workers manage their own subscribers
        // When a subscriber disconnects, the worker detects it and removes it
    }

    /// Broadcast message to all matching subscribers across all workers.
    ///
    /// Message is shared via Arc for zero-copy distribution to workers.
    /// Each worker filters by subscription prefix and delivers to matching subscribers only.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        // Check poison flag first
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket is poisoned from previous incomplete operation",
            ));
        }

        if msg.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty message"));
        }

        // Create guard to poison socket if we panic or cancel mid-broadcast
        let guard = PoisonGuard::new(&mut self.is_poisoned);

        trace!("[PUB] Broadcasting to {} workers", self.workers.len());

        // Wrap message in Arc for zero-copy sharing across workers
        let message = Arc::new(msg);

        // Try to send to each worker. Use try_send (non-blocking) so a slow
        // worker doesn't stall the broadcast. If the channel is full (HWM
        // reached), drop the message for that worker and count it.
        for (idx, worker) in self.workers.iter().enumerate() {
            match worker.try_send(WorkerCommand::Broadcast {
                message: message.clone(),
            }) {
                Ok(()) => {}
                Err(flume::TrySendError::Full(_)) => {
                    self.drop_count.fetch_add(1, Ordering::Relaxed);
                    debug!("[PUB] Worker {} channel full (HWM), message dropped", idx);
                }
                Err(flume::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other(format!(
                        "Worker {} channel disconnected",
                        idx
                    )));
                }
            }
        }

        guard.disarm();
        Ok(())
    }

    /// Get subscriber count.
    #[inline]
    pub const fn subscriber_count(&self) -> usize {
        self.subscriber_count
    }

    /// Number of messages dropped due to full worker channels (HWM backpressure).
    ///
    /// Increments when `send()` calls `try_send` on a worker channel that is at
    /// capacity. Reset by creating a new socket  -  this counter is never cleared.
    #[inline]
    pub fn drop_count(&self) -> u64 {
        self.drop_count.load(Ordering::Relaxed)
    }

    /// Get socket options
    #[inline]
    pub const fn options(&self) -> &SocketOptions {
        &self.options
    }

    /// Get mutable socket options
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.options
    }

    /// Get the socket type.
    ///
    /// # ZeroMQ Compatibility
    ///
    /// Corresponds to `ZMQ_TYPE` (16) option.
    #[inline]
    pub const fn socket_type(&self) -> SocketType {
        SocketType::Pub
    }
}

impl Default for PubSocket {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PubSocket {
    fn drop(&mut self) {
        debug!("[PUB] Shutting down {} workers", self.workers.len());
        for worker in &self.workers {
            let _ = worker.send(WorkerCommand::Shutdown);
        }
    }
}

// Implement Socket trait for PubSocket (non-generic)
#[async_trait::async_trait(?Send)]
impl crate::Socket for PubSocket {
    async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        self.send(msg).await
    }

    async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "PUB sockets do not support receive operations",
        ))
    }

    fn socket_type(&self) -> SocketType {
        SocketType::Pub
    }
}
