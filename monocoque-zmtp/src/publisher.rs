/// High-performance PUB socket implementation with worker pool.
///
/// This module provides a scalable PUB socket that can broadcast to multiple
/// subscribers with minimal overhead using:
/// - **Worker pool architecture**: Multiple threads handle subscribers in parallel
/// - **io_uring per worker**: Each worker thread runs its own compio runtime with io_uring
/// - **Zero-copy message broadcasting**: Shared `Arc<Bytes>` across all workers
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
/// - Zero-copy via `Arc<Bytes>` for message data
use bytes::Bytes;
use compio::net::{OwnedReadHalf, OwnedWriteHalf, TcpListener, TcpStream};
use flume::{Receiver, Sender};
use monocoque_core::subscription::SubscriptionEvent;

use crate::handshake::perform_handshake_with_options;
use crate::session::SocketType;
use monocoque_core::options::SocketOptions;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use tracing::{debug, error, trace, warn};

/// Maximum number of broadcasts coalesced into a single per-subscriber write.
///
/// When the producer outpaces a worker, consecutive broadcasts queue up. The
/// worker drains up to this many into one batch so each subscriber gets one
/// `write_all` per burst instead of one write per message. Because the worker
/// awaits each write, throughput scales with messages-per-write, so this is set
/// high enough to drain a full channel (HWM) in a single batch under load. The
/// worker only ever drains what is already queued, so low-rate publishers still
/// flush immediately (no added latency).
const MAX_COALESCE_MSGS: usize = 4096;

/// Soft byte cap on a coalesced batch. Draining stops once the accumulated wire
/// size reaches this, bounding per-subscriber memory for large messages.
const COALESCE_BYTE_LIMIT: usize = 4 * 1024 * 1024;

/// Unique identifier for each subscriber connection
type SubscriberId = u64;

/// Union of all subscribers' subscriptions, used by `send()` to drop a broadcast
/// *before* the per-message `Arc` + cross-thread hand-off when no subscriber
/// could possibly want it. This is the big lever for topic-filtered workloads,
/// where most published messages match nobody.
///
/// `match_all` counts subscribers in the "no explicit subscription = receive
/// everything" state (a freshly connected subscriber, matching the worker's
/// `matches()` semantics). `prefixes` refcounts distinct subscription prefixes
/// (a `b""` prefix, i.e. an explicit subscribe-to-all, matches every topic).
/// Matching is an allocation-free linear prefix scan, which is fast for the
/// handful of distinct prefixes a PUB socket typically sees.
///
/// Disconnect cleanup is intentionally omitted: a stale entry only makes the
/// union match *more* (less prefiltering), never less, so it can never drop a
/// message a live subscriber wanted.
#[derive(Default, Clone)]
struct SubscriptionUnion {
    match_all: usize,
    prefixes: Vec<(Vec<u8>, usize)>,
}

impl SubscriptionUnion {
    /// A newly connected subscriber starts with no subscription = match all.
    fn add_subscriber(&mut self) {
        self.match_all += 1;
    }

    /// Record a subscription. `sub_was_empty` is true when this is the
    /// subscriber's first prefix (it leaves the match-all state).
    fn subscribe(&mut self, prefix: &[u8], sub_was_empty: bool) {
        if sub_was_empty {
            self.match_all = self.match_all.saturating_sub(1);
        }
        if let Some(entry) = self.prefixes.iter_mut().find(|(p, _)| p == prefix) {
            entry.1 += 1;
        } else {
            self.prefixes.push((prefix.to_vec(), 1));
        }
    }

    /// Drop a subscription. `sub_now_empty` is true when the subscriber has no
    /// prefixes left (it returns to the match-all state).
    fn unsubscribe(&mut self, prefix: &[u8], sub_now_empty: bool) {
        if let Some(pos) = self.prefixes.iter().position(|(p, _)| p == prefix) {
            self.prefixes[pos].1 -= 1;
            if self.prefixes[pos].1 == 0 {
                self.prefixes.swap_remove(pos);
            }
        }
        if sub_now_empty {
            self.match_all += 1;
        }
    }

    /// Could any subscriber want a message with this first frame?
    #[inline]
    fn matches(&self, topic: &[u8]) -> bool {
        self.match_all > 0
            || self
                .prefixes
                .iter()
                .any(|(p, _)| topic.starts_with(p.as_slice()))
    }
}

/// The subscription union shared between the publisher and its subscription
/// reader tasks, paired with a generation counter.
///
/// Profiling showed a `RwLock` read on the prefilter hot path costs ~22 ns vs
/// ~4 ns for the raw match (and a lock-free `ArcSwap` load was no cheaper at
/// ~20 ns). Subscriptions change very rarely, so the single-threaded publisher
/// keeps a private cached copy and only re-reads under the lock when `generation`
/// changes — collapsing the steady-state cost to one relaxed atomic load plus
/// the raw scan. Writers bump `generation` (Release) after updating `union`.
struct SharedSubscriptions {
    union: RwLock<SubscriptionUnion>,
    generation: AtomicU64,
}

impl SharedSubscriptions {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            union: RwLock::new(SubscriptionUnion::default()),
            generation: AtomicU64::new(0),
        })
    }

    /// Apply a mutation to the union and publish a new generation.
    fn update(&self, f: impl FnOnce(&mut SubscriptionUnion)) {
        f(&mut self.union.write());
        self.generation.fetch_add(1, Ordering::Release);
    }
}

/// Shared subscription state (updated by subscription reader, read by broadcast sender)
type SubscriptionState = Arc<RwLock<Vec<Bytes>>>;

type SubCipher = Arc<parking_lot::Mutex<crate::security::curve::CurveMessageCipher>>;

/// Commands sent from main thread to worker threads
enum WorkerCommand {
    /// Add a new subscriber to this worker
    AddSubscriber {
        id: SubscriberId,
        stream: TcpStream,
        subscriptions: SubscriptionState,
        cipher: Option<SubCipher>,
        /// Resolved `max_msg_size` for the subscription reader's decoder.
        max_frame_size: Option<usize>,
        /// Shared subscription union for the `send()` prefilter.
        union: Arc<SharedSubscriptions>,
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
    cipher: Option<SubCipher>,
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
    cipher: Option<SubCipher>,
    max_frame_size: Option<usize>,
    union: Arc<SharedSubscriptions>,
) {
    use compio::buf::BufResult;
    use compio::io::AsyncRead;
    use monocoque_core::buffer::SegmentedBuffer;

    trace!("[PUB] Subscription reader started for subscriber {}", id);

    let mut recv_buf = SegmentedBuffer::new();
    let mut decoder = max_frame_size.map_or_else(
        crate::codec::ZmtpDecoder::new,
        crate::codec::ZmtpDecoder::with_max_frame_size,
    );

    // Allocate the read buffer once and reuse it each iteration.
    // compio returns ownership via BufResult, so we hand it back in on every read.
    let mut buf = vec![0u8; 256];
    loop {
        let BufResult(result, returned_buf) = reader.read(buf).await;
        buf = returned_buf;

        match result {
            Ok(0) => {
                debug!("[PUB] Subscriber {} disconnected (subscription reader)", id);
                break;
            }
            Ok(n) => {
                recv_buf.push(Bytes::copy_from_slice(&buf[..n]));

                // Drain all complete ZMTP frames from the accumulated buffer.
                loop {
                    match decoder.decode(&mut recv_buf) {
                        Ok(Some(frame)) => {
                            // Resolve payload: decrypt CURVE MESSAGE frames, skip other commands.
                            let payload = if frame.is_command() {
                                if let Some(ref arc_cipher) = cipher {
                                    if crate::security::curve::CurveMessageCipher::is_curve_message(
                                        &frame.payload,
                                    ) {
                                        let mut cipher_guard = arc_cipher.lock();
                                        match cipher_guard.decrypt_frame(&frame.payload) {
                                            Ok((_more, data)) => data,
                                            Err(_) => continue,
                                        }
                                    } else {
                                        continue;
                                    }
                                } else {
                                    continue;
                                }
                            } else if cipher.is_some() {
                                // Reject plaintext data frames when CURVE is active.
                                break;
                            } else {
                                frame.payload
                            };

                            if let Some(event) = SubscriptionEvent::from_message(&payload) {
                                let mut subs = subscriptions.write();
                                match event {
                                    SubscriptionEvent::Subscribe(prefix) => {
                                        if !subs.contains(&prefix) {
                                            trace!(
                                                "[PUB] Subscriber {} subscribed to {:?}",
                                                id, prefix
                                            );
                                            let was_empty = subs.is_empty();
                                            union.update(|u| u.subscribe(&prefix, was_empty));
                                            subs.push(prefix);
                                        }
                                    }
                                    SubscriptionEvent::Unsubscribe(prefix) => {
                                        let before = subs.len();
                                        subs.retain(|s| s != &prefix);
                                        if subs.len() < before {
                                            trace!(
                                                "[PUB] Subscriber {} unsubscribed from {:?}",
                                                id, prefix
                                            );
                                            let now_empty = subs.is_empty();
                                            union.update(|u| u.unsubscribe(&prefix, now_empty));
                                        }
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
#[allow(clippy::too_many_lines)]
fn worker_thread(worker_id: usize, rx: Receiver<WorkerCommand>, sub_count: Arc<AtomicUsize>) {
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
                    cipher,
                    max_frame_size,
                    union,
                }) => {
                    add_subscriber(
                        &mut subscribers,
                        worker_id,
                        id,
                        stream,
                        subscriptions,
                        cipher,
                        max_frame_size,
                        union,
                    );
                }
                Ok(WorkerCommand::Broadcast { message }) => {
                    // Coalescing: drain any broadcasts already queued behind this
                    // one into a single batch, so each subscriber receives one
                    // vectored write per burst instead of one write per message.
                    let mut batch: Vec<Arc<Vec<Bytes>>> = Vec::with_capacity(4);
                    let mut batch_bytes = approx_wire_len(&message);
                    batch.push(message);

                    let mut deferred: Option<WorkerCommand> = None;
                    while batch.len() < MAX_COALESCE_MSGS && batch_bytes < COALESCE_BYTE_LIMIT {
                        match rx.try_recv() {
                            Ok(WorkerCommand::Broadcast { message }) => {
                                batch_bytes += approx_wire_len(&message);
                                batch.push(message);
                            }
                            // A non-broadcast command interrupts the burst: stash
                            // it and process after flushing this batch so command
                            // ordering is preserved.
                            Ok(other) => {
                                deferred = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }

                    trace!(
                        "[Worker {}] Broadcasting batch of {} to {} subscribers",
                        worker_id,
                        batch.len(),
                        subscribers.len()
                    );

                    // Contiguous whole-batch wire, built once and shared (O(1)
                    // clone) by every plaintext subscribe-to-all subscriber. A
                    // contiguous buffer + one `write_all` beats a many-segment
                    // vectored write for the small messages typical of PUB/SUB
                    // (see the vectored-write crossover in BENCHMARKS.md).
                    let mut full_batch: Option<Bytes> = None;
                    // Per-message wire, shared across partial (topic) matchers.
                    let mut plain_wire: Vec<Option<Bytes>> = vec![None; batch.len()];

                    let mut dead_subs = Vec::new();
                    for sub in subscribers.values_mut() {
                        let plaintext_all =
                            sub.cipher.is_none() && sub.subscriptions.read().is_empty();

                        // Encode this subscriber's slice of the batch into one
                        // contiguous buffer.
                        let out: Bytes = if plaintext_all {
                            full_batch
                                .get_or_insert_with(|| {
                                    let mut buf = bytes::BytesMut::new();
                                    for msg in &batch {
                                        crate::codec::encode_multipart(msg, &mut buf);
                                    }
                                    buf.freeze()
                                })
                                .clone()
                        } else {
                            let mut buf = bytes::BytesMut::new();
                            let mut failed = false;
                            for (idx, msg) in batch.iter().enumerate() {
                                if !sub.matches(msg) {
                                    continue;
                                }
                                if let Some(ref arc_cipher) = sub.cipher {
                                    if let Some(wire) = encode_curve_wire(msg, arc_cipher) {
                                        buf.extend_from_slice(&wire);
                                    } else {
                                        failed = true;
                                        break;
                                    }
                                } else {
                                    let wire = plain_wire[idx]
                                        .get_or_insert_with(|| encode_plain_wire(msg));
                                    buf.extend_from_slice(wire);
                                }
                            }
                            if failed {
                                dead_subs.push(sub.id);
                                continue;
                            }
                            buf.freeze()
                        };

                        if out.is_empty() {
                            continue; // nothing in this batch matched
                        }

                        // Send with 5-second timeout for fault isolation.
                        let send_result = compio::time::timeout(
                            std::time::Duration::from_secs(5),
                            send_all_to_stream(&mut sub.stream, out),
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
                    if !dead_subs.is_empty() {
                        sub_count.fetch_sub(dead_subs.len(), Ordering::Relaxed);
                    }
                    for id in dead_subs {
                        subscribers.remove(&id);
                        debug!("[Worker {}] Removed dead subscriber {}", worker_id, id);
                    }

                    // Process any command pulled off the queue during coalescing.
                    match deferred {
                        Some(WorkerCommand::AddSubscriber {
                            id,
                            stream,
                            subscriptions,
                            cipher,
                            max_frame_size,
                            union,
                        }) => {
                            add_subscriber(
                                &mut subscribers,
                                worker_id,
                                id,
                                stream,
                                subscriptions,
                                cipher,
                                max_frame_size,
                                union,
                            );
                        }
                        Some(WorkerCommand::Shutdown) => {
                            debug!("[Worker {}] Shutting down", worker_id);
                            break;
                        }
                        // Broadcasts are always drained into the batch above.
                        Some(WorkerCommand::Broadcast { .. }) | None => {}
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

/// Write a subscriber's contiguous batch wire in a single `write_all`.
///
/// `data` is the whole coalesced burst encoded into one buffer (shared O(1) via
/// `Bytes::clone` across plaintext subscribe-to-all subscribers). One contiguous
/// write is faster than a many-segment vectored write for small PUB/SUB messages.
async fn send_all_to_stream(stream: &mut OwnedWriteHalf<TcpStream>, data: Bytes) -> io::Result<()> {
    use compio::buf::BufResult;
    use compio::io::AsyncWriteExt;
    let BufResult(res, _) = stream.write_all(data).await;
    res
}

/// Encode one plaintext message to its complete ZMTP wire bytes.
fn encode_plain_wire(msg: &[Bytes]) -> Bytes {
    let mut buf = bytes::BytesMut::new();
    crate::codec::encode_multipart(msg, &mut buf);
    buf.freeze()
}

/// Encrypt one message into its complete CURVE wire bytes for a subscriber.
///
/// Returns `None` if any frame fails to encrypt (the caller drops the
/// subscriber).
fn encode_curve_wire(msg: &[Bytes], cipher: &SubCipher) -> Option<Bytes> {
    let last = msg.len().saturating_sub(1);
    let mut buf = bytes::BytesMut::new();
    let mut cipher = cipher.lock();
    for (i, frame) in msg.iter().enumerate() {
        let body = cipher.encrypt_frame(frame, i < last).ok()?;
        crate::base::append_zmtp_cmd_frame(&mut buf, &body);
    }
    Some(buf.freeze())
}

/// Approximate wire size of a message (frame bodies + per-frame header budget),
/// used only to bound the coalesced batch size.
fn approx_wire_len(msg: &[Bytes]) -> usize {
    msg.iter().map(|f| f.len() + 9).sum()
}

/// Register a new subscriber with this worker: split its stream, spawn the
/// subscription reader, and record the write half.
#[allow(clippy::too_many_arguments)]
fn add_subscriber(
    subscribers: &mut HashMap<SubscriberId, WorkerSubscriber>,
    worker_id: usize,
    id: SubscriberId,
    stream: TcpStream,
    subscriptions: SubscriptionState,
    cipher: Option<SubCipher>,
    max_frame_size: Option<usize>,
    union: Arc<SharedSubscriptions>,
) {
    debug!("[Worker {}] Adding subscriber {}", worker_id, id);

    // Split stream into read half (subscriptions) and write half (broadcasts).
    let (read_half, write_half) = stream.into_split();

    // Spawn background task to read subscription messages; runs concurrently
    // within this worker's compio runtime.
    let sub_state = Arc::clone(&subscriptions);
    let reader_cipher = cipher.clone();
    compio::runtime::spawn(subscription_reader(
        id,
        read_half,
        sub_state,
        reader_cipher,
        max_frame_size,
        union,
    ))
    .detach();

    subscribers.insert(
        id,
        WorkerSubscriber {
            id,
            stream: write_half,
            subscriptions,
            cipher,
        },
    );
}

/// PUB socket with worker pool for multi-subscriber broadcasting
///
/// Uses multiple worker threads to handle subscribers in parallel.
/// Each worker runs its own compio runtime with io_uring.
pub struct PubSocket {
    /// Worker thread channels (bounded by send_hwm for backpressure)
    workers: Vec<Sender<WorkerCommand>>,
    /// Live subscriber count per worker. `send()` skips workers at zero so a
    /// low-subscriber broadcast does not pay a channel hand-off to every worker.
    worker_sub_counts: Vec<Arc<AtomicUsize>>,
    /// Union of all subscriptions, kept current by the subscription readers.
    /// `send()` consults it to drop a broadcast before the hand-off when no
    /// subscriber could match (the big win for topic-filtered workloads).
    subscription_union: Arc<SharedSubscriptions>,
    /// Publisher-local cached copy of the union and the generation it reflects.
    /// The prefilter reads this without locking; it is refreshed (under the
    /// lock) only when the shared generation advances.
    local_union: SubscriptionUnion,
    local_gen: u64,
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
        let mut worker_sub_counts = Vec::with_capacity(worker_count);

        for i in 0..worker_count {
            let (tx, rx) = flume::bounded(hwm);
            let sub_count = Arc::new(AtomicUsize::new(0));
            let worker_count = Arc::clone(&sub_count);
            thread::Builder::new()
                .name(format!("pub-worker-{}", i))
                .spawn(move || worker_thread(i, rx, worker_count))
                .expect("Failed to spawn worker thread");
            workers.push(tx);
            worker_sub_counts.push(sub_count);
        }

        Self {
            workers,
            worker_sub_counts,
            subscription_union: SharedSubscriptions::new(),
            local_union: SubscriptionUnion::default(),
            local_gen: 0,
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

        // Assign to next worker (round-robin). Bump the worker's live-subscriber
        // count now (before the AddSubscriber is processed) so a concurrent
        // broadcast is never skipped for this worker.
        let worker_idx = self.next_worker;
        self.next_worker = (self.next_worker + 1) % self.workers.len();
        self.worker_sub_counts[worker_idx].fetch_add(1, Ordering::Relaxed);
        // A new subscriber with no subscription yet matches everything, so the
        // prefilter must not drop anything until it narrows its interest.
        self.subscription_union
            .update(SubscriptionUnion::add_subscriber);

        debug!("[PUB] Assigning subscriber {} to worker {}", id, worker_idx);

        let cipher = handshake_result
            .curve_cipher
            .map(|c| Arc::new(parking_lot::Mutex::new(c)));

        // Send stream + subscriptions + cipher to worker. The worker will split the stream
        // into read (subscription reader task) and write (broadcast) halves.
        self.workers[worker_idx]
            .send_async(WorkerCommand::AddSubscriber {
                id,
                stream,
                subscriptions,
                cipher,
                max_frame_size: self.options.max_msg_size,
                union: Arc::clone(&self.subscription_union),
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

    /// Remove a subscriber and decrement the subscriber count.
    ///
    /// Workers also evict dead subscribers automatically when a send error is
    /// detected, but callers that track disconnections explicitly should call
    /// this method so that `subscriber_count()` stays accurate.
    pub fn remove_subscriber(&mut self, _id: SubscriberId) {
        self.subscriber_count = self.subscriber_count.saturating_sub(1);
    }

    /// Could any subscriber want a message with this first frame? Cheap and
    /// allocation-free; `invert_matching` disables the prefilter (the union
    /// models normal matching only).
    ///
    /// Steady-state cost is one relaxed atomic load plus a raw prefix scan; the
    /// shared union is re-read under its lock only when the generation advances
    /// (i.e. a subscription changed), which is rare relative to publishing.
    #[inline]
    fn prefilter_allows(&mut self, topic: &[u8]) -> bool {
        if self.options.invert_matching {
            return true;
        }
        let g = self.subscription_union.generation.load(Ordering::Acquire);
        if g != self.local_gen {
            self.local_union = self.subscription_union.union.read().clone();
            self.local_gen = g;
        }
        self.local_union.matches(topic)
    }

    /// Hand a shared message to every worker that holds subscribers.
    ///
    /// `try_send` is non-blocking so a slow worker can't stall the broadcast;
    /// a full channel (HWM) drops the message for that worker and counts it.
    ///
    /// No [`PoisonGuard`] here: this path has no `.await` (the broadcast is a
    /// non-blocking `try_send` to the worker threads, and the actual TCP writes
    /// happen on those workers), so it can't be cancelled mid-flight and writes
    /// to no stream. **If this hand-off ever gains a suspension point** (e.g. an
    /// awaiting `send_async` for backpressure instead of HWM-dropping), wrap the
    /// loop in a `PoisonGuard` like the PUSH/DEALER/REP write paths do.
    fn dispatch(&self, message: &Arc<Vec<Bytes>>) -> io::Result<()> {
        for (idx, worker) in self.workers.iter().enumerate() {
            // Skip workers with no subscribers: no point paying the channel
            // hand-off + Arc clone for a worker that will match nothing.
            if self.worker_sub_counts[idx].load(Ordering::Relaxed) == 0 {
                continue;
            }
            match worker.try_send(WorkerCommand::Broadcast {
                message: Arc::clone(message),
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
        Ok(())
    }

    /// Broadcast a message to all matching subscribers across all workers.
    ///
    /// The message is shared via `Arc` for zero-copy distribution to workers;
    /// each worker filters by subscription and delivers to matching subscribers
    /// only. See [`send_frames`](Self::send_frames) for an allocation-light
    /// variant that publishes from borrowed frames.
    pub async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()> {
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket is poisoned from previous incomplete operation",
            ));
        }
        if msg.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty message"));
        }
        if !self.prefilter_allows(msg.first().map_or(&[][..], |f| f.as_ref())) {
            return Ok(());
        }
        self.dispatch(&Arc::new(msg))
    }

    /// Broadcast a message given as borrowed frames.
    ///
    /// Identical to [`send`](Self::send) but allocates the shared message **only
    /// when it actually matches a subscription**. Publishing from a stack array,
    /// `send_frames(&[topic, payload])`, pays no per-message heap allocation on
    /// the common drop path of a topic-filtered stream, only when a subscriber
    /// wants the message. This is the low-overhead path for high-rate publishers.
    pub async fn send_frames(&mut self, frames: &[Bytes]) -> io::Result<()> {
        if self.is_poisoned {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Socket is poisoned from previous incomplete operation",
            ));
        }
        if frames.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty message"));
        }
        if !self.prefilter_allows(frames.first().map_or(&[][..], |f| f.as_ref())) {
            return Ok(());
        }
        self.dispatch(&Arc::new(frames.to_vec()))
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
