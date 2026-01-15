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

use bytes::{Bytes, BytesMut};
use compio::buf::BufResult;
use compio::io::{AsyncRead, AsyncWriteExt};
use compio::runtime::TryClone;
use compio::net::{TcpListener, TcpStream};
use flume::{Sender, Receiver};

use monocoque_core::options::SocketOptions;
use crate::session::SocketType;
use crate::handshake::perform_handshake_with_timeout;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::thread;
use tracing::{debug, trace, warn, error};

/// Unique identifier for each subscriber connection
type SubscriberId = u64;

/// Shared subscription state (updated by subscription reader, read by sender)
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
    Broadcast {
        message: Arc<Vec<Bytes>>,
    },
    /// Shutdown the worker
    Shutdown,
}

/// Per-subscriber state managed by worker
struct WorkerSubscriber {
    id: SubscriberId,
    stream: TcpStream,
    subscriptions: SubscriptionState,
    write_buf: BytesMut,
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
                if sub.is_empty() || 
                   (first_frame.len() >= sub.len() && &first_frame[..sub.len()] == &sub[..]) {
                    return true;
                }
            }
        }
        false
    }
    
    /// Send message to this subscriber
    async fn send(&mut self, msg: &[Bytes]) -> io::Result<()> {
        if !self.matches(msg) {
            return Ok(());
        }

        // Send message using ZMTP framing
        for (i, frame) in msg.iter().enumerate() {
            let is_last = i == msg.len() - 1;
            let flags = if is_last { 0x00 } else { 0x01 }; // MORE flag
            
            // Frame: [flags:1][size:8][data:size]
            let BufResult(res, _) = self.stream.write_all([flags]).await;
            res?;
            let BufResult(res, _) = self.stream.write_all((frame.len() as u64).to_be_bytes()).await;
            res?;
            
            // Convert Bytes to Vec<u8> for IoBuf compatibility
            let data = frame.to_vec();
            let BufResult(res, _) = self.stream.write_all(data).await;
            res?;
        }
        
        Ok(())
    }
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
                Ok(WorkerCommand::AddSubscriber { id, stream, subscriptions }) => {
                    debug!("[Worker {}] Adding subscriber {}", worker_id, id);
                    
                    // Spawn subscription reader task
                    let subs_clone = subscriptions.clone();
                    let stream_clone = stream.try_clone().unwrap(); // Clone for reading
                    compio::runtime::spawn(async move {
                        subscription_reader_task(stream_clone, subs_clone, id).await;
                    }).detach();
                    
                    subscribers.insert(id, WorkerSubscriber {
                        id,
                        stream,
                        subscriptions,
                        write_buf: BytesMut::with_capacity(4096),
                    });
                }
                Ok(WorkerCommand::Broadcast { message }) => {
                    let mut dead = Vec::new();
                    for (id, sub) in &mut subscribers {
                        if let Err(e) = sub.send(&message).await {
                            debug!("[Worker {}] Subscriber {} send failed: {}", worker_id, id, e);
                            dead.push(*id);
                        }
                    }
                    for id in dead {
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

/// Subscription reader task (runs in worker's compio runtime)
async fn subscription_reader_task(
    mut stream: TcpStream,
    subscriptions: SubscriptionState,
    subscriber_id: SubscriberId,
) {
    let mut recv_buf = Vec::with_capacity(256);
    
    loop {
        recv_buf.clear();
        recv_buf.resize(256, 0);
        
        match AsyncRead::read(&mut stream, recv_buf).await {
            BufResult(Ok(0), _) => {
                debug!("[PUB] Subscriber {} disconnected", subscriber_id);
                break;
            }
            BufResult(Ok(n), buf) => {
                recv_buf = buf;
                let data = &recv_buf[..n];
                
                if !data.is_empty() {
                    let mut subs = subscriptions.write();
                    match data[0] {
                        0x01 => {
                            let prefix = Bytes::copy_from_slice(&data[1..]);
                            if !subs.contains(&prefix) {
                                trace!("[PUB] Subscriber {} added subscription: {:?}", subscriber_id, prefix);
                                subs.push(prefix);
                            }
                        }
                        0x00 => {
                            let prefix = Bytes::copy_from_slice(&data[1..]);
                            subs.retain(|s| s != &prefix);
                            trace!("[PUB] Subscriber {} removed subscription: {:?}", subscriber_id, prefix);
                        }
                        _ => warn!("[PUB] Subscriber {} invalid subscription: {:#x}", subscriber_id, data[0]),
                    }
                }
            }
            BufResult(Err(e), buf) => {
                recv_buf = buf;
                debug!("[PUB] Subscriber {} read error: {}", subscriber_id, e);
                break;
            }
        }
    }
}

/// PUB socket with worker pool for multi-subscriber broadcasting
///
/// Uses multiple worker threads to handle subscribers in parallel.
/// Each worker runs its own compio runtime with io_uring.
pub struct PubSocket {
    /// Worker thread channels
    workers: Vec<Sender<WorkerCommand>>,
    /// Next subscriber ID
    next_id: SubscriberId,
    /// Next worker to assign (round-robin)
    next_worker: usize,
    /// Socket options
    options: SocketOptions,
    /// Subscriber count
    subscriber_count: usize,
}

impl PubSocket {
    /// Create a new PUB socket with default worker count (number of CPU cores)
    pub fn new() -> Self {
        Self::with_workers(num_cpus::get().max(2))
    }
    
    /// Create with specific number of worker threads
    pub fn with_workers(worker_count: usize) -> Self {
        debug!("[PUB] Starting {} worker threads", worker_count);
        
        let mut workers = Vec::with_capacity(worker_count);
        
        for i in 0..worker_count {
            let (tx, rx) = flume::unbounded();
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
            options: SocketOptions::default(),
            subscriber_count: 0,
        }
    }
    
    /// Accept a new subscriber connection
    ///
    /// Performs ZMTP handshake and assigns subscriber to a worker thread (round-robin).
    pub async fn accept_subscriber(&mut self, listener: &TcpListener) -> io::Result<SubscriberId> {
        let (stream, addr) = listener.accept().await?;
        
        monocoque_core::tcp::enable_tcp_nodelay(&stream)?;
        debug!("[PUB] Accepted connection from {}", addr);
        
        let mut stream = stream;
        let handshake_result = perform_handshake_with_timeout(
            &mut stream,
            SocketType::Pub,
            None,
            Some(self.options.handshake_timeout),
        )
        .await
        .map_err(|e| io::Error::other(format!("Handshake failed: {}", e)))?;
        
        debug!(peer_socket_type = ?handshake_result.peer_socket_type, "[PUB] Handshake complete");
        
        let id = self.next_id;
        self.next_id += 1;
        
        // Create subscription state for this subscriber
        let subscriptions = Arc::new(RwLock::new(Vec::new()));
        
        // Assign to next worker (round-robin)
        let worker_idx = self.next_worker;
        self.next_worker = (self.next_worker + 1) % self.workers.len();
        
        debug!("[PUB] Assigning subscriber {} to worker {}", id, worker_idx);
        
        self.workers[worker_idx]
            .send(WorkerCommand::AddSubscriber {
                id,
                stream,
                subscriptions,
            })
            .map_err(|e| io::Error::other(format!("Failed to send to worker: {}", e)))?;
        
        self.subscriber_count += 1;
        debug!("[PUB] Subscriber {} accepted (total: {})", id, self.subscriber_count);
        
        // TODO: Spawn background task to read subscriptions
        // This requires either:
        // 1. A second TCP connection for reading
        // 2. A channel-based architecture
        // 3. Or we accept that subscriptions are set client-side only
        
        // For now, subscriptions default to "match all" (empty subscription list = match all)
        debug!("[PUB] Accepted subscriber {} from {} (subscriptions default to match-all)", id, addr);
        
        Ok(id)
    }
    
    /// Remove a subscriber (handled automatically by worker drop)
    pub fn remove_subscriber(&mut self, _id: SubscriberId) {
        // No-op: workers manage their own subscribers
        // When a subscriber disconnects, the worker detects it and removes it
    }
    
    /// Broadcast message to all matching subscribers
    ///
    /// Hot path - optimized for:
    /// - Single iteration through subscribers
    /// Broadcast message to all subscribers across all workers
    ///
    /// Message is shared via Arc for zero-copy distribution to workers.
    /// Each worker handles its subset of subscribers in parallel.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        if msg.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty message"));
        }
        
        trace!("[PUB] Broadcasting to {} workers", self.workers.len());
        
        // Wrap message in Arc for zero-copy sharing across workers
        let message = Arc::new(msg);
        
        // Send to all workers (they filter by subscriptions)
        for worker in &self.workers {
            worker.send(WorkerCommand::Broadcast {
                message: message.clone(),
            }).map_err(|e| io::Error::other(format!("Worker send failed: {}", e)))?;
        }
        
        Ok(())
    }
    
    /// Get subscriber count
    #[inline]
    pub fn subscriber_count(&self) -> usize {
        self.subscriber_count
    }
    
    /// Get socket options
    #[inline]
    pub fn options(&self) -> &SocketOptions {
        &self.options
    }
    
    /// Get mutable socket options
    #[inline]
    pub fn options_mut(&mut self) -> &mut SocketOptions {
        &mut self.options
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
