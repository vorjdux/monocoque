# Production Deployment Guide

**Version:** 0.1.0  
**Target:** Production environments  
**Last Updated:** January 25, 2026

---

## Table of Contents

1. [Pre-Deployment Checklist](#pre-deployment-checklist)
2. [Architecture Recommendations](#architecture-recommendations)
3. [Security Configuration](#security-configuration)
4. [Performance Tuning](#performance-tuning)
5. [Monitoring & Observability](#monitoring--observability)
6. [Error Handling & Recovery](#error-handling--recovery)
7. [Migration from libzmq](#migration-from-libzmq)
8. [Deployment Patterns](#deployment-patterns)
9. [Troubleshooting](#troubleshooting)

---

## Pre-Deployment Checklist

### ✅ Essential Requirements

- [ ] **Rust version:** 1.75+ installed
- [ ] **Security review:** Complete security audit
- [ ] **Testing:** All integration tests passing
- [ ] **Load testing:** Performance validated under production load
- [ ] **Monitoring:** Metrics collection configured
- [ ] **Error handling:** All error paths tested
- [ ] **Documentation:** Architecture documented
- [ ] **Rollback plan:** Tested rollback procedure

### ✅ Security Checklist

- [ ] **CURVE encryption:** Enabled for all external connections
- [ ] **ZAP authentication:** Configured and tested
- [ ] **Credentials:** Rotated and stored securely (Vault, k8s secrets)
- [ ] **TLS:** Used for control plane if applicable
- [ ] **Network policies:** Firewall rules configured
- [ ] **Audit logging:** Authentication attempts logged

### ✅ Performance Checklist

- [ ] **Buffer sizes:** Tuned for message sizes
- [ ] **Connection pooling:** Implemented if needed
- [ ] **TCP keepalive:** Configured appropriately
- [ ] **Resource limits:** Set (file descriptors, memory)
- [ ] **Benchmarks:** Run against production-like load

---

## Architecture Recommendations

### Single Service Architecture

```
┌────────────────┐
│   Service A    │
│  (REQ client)  │
└───────┬────────┘
        │ CURVE
        ▼
┌────────────────┐
│   Service B    │
│  (REP server)  │
└────────────────┘
```

**Use when:**
- Simple request/reply pattern
- Low to moderate throughput (<100k msg/s)
- Single datacenter

**Configuration:**
```rust
let mut options = SocketOptions::default();
options.tcp_keepalive = true;
options.tcp_keepalive_idle = 60;    // 60s idle before keepalive
options.tcp_keepalive_interval = 10; // 10s between probes
options.handshake_timeout = Duration::from_secs(5);
```

### High-Throughput Architecture

```
┌──────────┐  ┌──────────┐  ┌──────────┐
│ Pub A    │  │ Pub B    │  │ Pub C    │
└────┬─────┘  └────┬─────┘  └────┬─────┘
     │             │             │
     └─────────────┼─────────────┘
                   │ Multicast
                   ▼
     ┌─────────────┴─────────────┐
     │             │             │
┌────▼─────┐  ┌────▼─────┐  ┌────▼─────┐
│ Sub 1    │  │ Sub 2    │  │ Sub 3    │
└──────────┘  └──────────┘  └──────────┘
```

**Use when:**
- Broadcasting to many subscribers
- High throughput (>1M msg/s)
- Low latency critical

**Configuration:**
```rust
let mut options = SocketOptions::large(); // 16KB buffers
options.req_relaxed = true; // If using REQ
options.tcp_nodelay = true; // Disable Nagle
```

### Multi-Region Architecture

```
Region A                Region B
┌──────────┐           ┌──────────┐
│ Service  │───────────│ Service  │
│ DEALER   │  CURVE    │ ROUTER   │
└──────────┘  VPN/TLS  └──────────┘
```

**Use when:**
- Geographic distribution
- Cross-datacenter communication
- High security requirements

**Configuration:**
```rust
// Use CURVE + connection pooling
let mut options = SocketOptions::default();
options.handshake_timeout = Duration::from_secs(30); // Longer for WAN
options.connect_timeout = Duration::from_secs(30);
options.tcp_keepalive = true;
options.tcp_keepalive_idle = 120; // Longer for WAN
```

---

## Security Configuration

### CURVE Encryption (Production Standard)

**Key Generation:**
```rust
use monocoque_zmtp::security::curve::CurveKeyPair;

// Generate server long-term keypair (do once, store securely)
let server_keypair = CurveKeyPair::generate();

// Store private key securely (Vault, k8s secret, etc.)
let private_key_hex = hex::encode(server_keypair.secret.as_bytes());

// Distribute public key to clients
let public_key_hex = hex::encode(server_keypair.public.as_bytes());
```

**Server Configuration:**
```rust
use monocoque_zmtp::security::curve::CurveServerHandshake;

// Load keypair from secure storage
let secret_bytes = hex::decode(&env::var("CURVE_SECRET_KEY")?)?;
let server_keypair = CurveKeyPair::from_bytes(...);

// Perform CURVE handshake
let handshake = CurveServerHandshake::new(server_keypair);
let socket = TcpSocket::connect_with_handshake(stream, "server", handshake).await?;
```

**Client Configuration:**
```rust
use monocoque_zmtp::security::curve::CurveClientHandshake;

// Load server's public key
let server_public_hex = env::var("SERVER_PUBLIC_KEY")?;
let server_public = CurvePublicKey::from_hex(&server_public_hex)?;

// Generate ephemeral client keypair
let client_keypair = CurveKeyPair::generate();

let handshake = CurveClientHandshake::new(client_keypair, server_public);
let socket = TcpSocket::connect_with_handshake(stream, "client", handshake).await?;
```

### ZAP Authentication

**Production ZAP Handler:**
```rust
use monocoque_zmtp::security::zap_handler::{ZapHandler, spawn_zap_server};
use monocoque_zmtp::security::zap::{ZapRequest, ZapResponse, ZapStatus};

struct ProductionZapHandler {
    auth_service: Arc<AuthenticationService>,
}

#[async_trait::async_trait(?Send)]
impl ZapHandler for ProductionZapHandler {
    async fn authenticate(&self, request: &ZapRequest) -> ZapResponse {
        // Log authentication attempt
        info!("ZAP auth: domain={}, address={}", request.domain, request.address);
        
        // Call authentication service
        match self.auth_service.verify(&request).await {
            Ok(user_id) => {
                info!("Auth success: user={}", user_id);
                ZapResponse::success(request.request_id.clone(), user_id)
            }
            Err(e) => {
                warn!("Auth failed: {}", e);
                ZapResponse::failure(request.request_id.clone(), e.to_string())
            }
        }
    }
}

// Start ZAP server on application startup
spawn_zap_server(Arc::new(ProductionZapHandler { ... }))?;
```

### Credential Management

**DO NOT:**
- ❌ Hardcode credentials in source code
- ❌ Commit credentials to git
- ❌ Log credentials (even in debug mode)
- ❌ Use PLAIN over untrusted networks

**DO:**
- ✅ Store credentials in Vault/k8s secrets
- ✅ Rotate credentials regularly (30-90 days)
- ✅ Use CURVE for all production traffic
- ✅ Implement rate limiting in ZAP handler
- ✅ Monitor failed authentication attempts

---

## Performance Tuning

### Buffer Sizing

**Small Messages (<1KB):**
```rust
let options = SocketOptions::small(); // 4KB buffers
```

**Medium Messages (1-10KB):**
```rust
let options = SocketOptions::default(); // 8KB buffers
```

**Large Messages (>10KB):**
```rust
let options = SocketOptions::large(); // 16KB buffers
```

**Custom Sizing:**
```rust
let mut options = SocketOptions::default();
options.recv_buffer_size = 32 * 1024; // 32KB
options.send_buffer_size = 32 * 1024;
```

### TCP Optimization

**Low Latency (REQ/REP):**
```rust
options.tcp_nodelay = true;  // Disable Nagle's algorithm
options.tcp_keepalive = true;
options.tcp_keepalive_idle = 30;
```

**High Throughput (PUB/SUB):**
```rust
options.tcp_nodelay = false; // Allow batching
options.send_buffer_size = 128 * 1024; // Larger buffers
```

### Connection Management

**Connection Pooling:**
```rust
use std::collections::HashMap;

struct ConnectionPool {
    connections: HashMap<String, ReqSocket>,
}

impl ConnectionPool {
    async fn get_or_create(&mut self, addr: &str) -> io::Result<&mut ReqSocket> {
        if !self.connections.contains_key(addr) {
            let stream = TcpStream::connect(addr).await?;
            let socket = ReqSocket::new(stream).await?;
            self.connections.insert(addr.to_string(), socket);
        }
        Ok(self.connections.get_mut(addr).unwrap())
    }
}
```

---

## Monitoring & Observability

### Metrics to Track

**Connection Metrics:**
- Active connections
- Connection establishment rate
- Connection failures
- Handshake duration

**Message Metrics:**
- Messages sent/received per second
- Message size distribution (histogram)
- Multipart message count
- Failed sends/receives

**Performance Metrics:**
- Send latency (p50, p95, p99)
- Receive latency
- Buffer utilization
- Memory usage (RSS)

### Prometheus Integration

```rust
use prometheus::{register_counter, register_histogram, Counter, Histogram};

lazy_static! {
    static ref MESSAGES_SENT: Counter = 
        register_counter!("monocoque_messages_sent_total", "Total messages sent").unwrap();
    
    static ref SEND_LATENCY: Histogram = 
        register_histogram!("monocoque_send_latency_seconds", "Send latency").unwrap();
}

// In your send code:
let start = Instant::now();
socket.send(msg).await?;
MESSAGES_SENT.inc();
SEND_LATENCY.observe(start.elapsed().as_secs_f64());
```

### Logging Best Practices

```rust
use tracing::{info, warn, error, debug};

// Log connection events
info!("Connected to server: addr={}, handshake_time={}ms", addr, elapsed_ms);

// Log authentication
info!("Authentication success: user={}, domain={}", user_id, domain);
warn!("Authentication failed: addr={}, reason={}", addr, reason);

// Log errors
error!("Send failed: error={}, retrying={}", err, retry_count);

// Debug logging (disable in production)
debug!("Received frame: size={}, more={}", frame.len(), frame.more());
```

---

## Error Handling & Recovery

### Connection Failures

```rust
use backoff::{ExponentialBackoff, backoff::Backoff};

async fn connect_with_retry(addr: &str) -> io::Result<ReqSocket> {
    let mut backoff = ExponentialBackoff::default();
    
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                return ReqSocket::new(stream).await;
            }
            Err(e) => {
                if let Some(duration) = backoff.next_backoff() {
                    warn!("Connection failed: {}, retrying in {:?}", e, duration);
                    tokio::time::sleep(duration).await;
                } else {
                    error!("Connection failed after max retries: {}", e);
                    return Err(e);
                }
            }
        }
    }
}
```

### Send/Receive Timeouts

```rust
use tokio::time::timeout;

async fn send_with_timeout(socket: &mut ReqSocket, msg: Vec<Bytes>) -> io::Result<()> {
    timeout(Duration::from_secs(5), socket.send(msg))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Send timeout"))?
}
```

### Circuit Breaker Pattern

```rust
struct CircuitBreaker {
    failures: AtomicUsize,
    threshold: usize,
    state: Mutex<State>,
}

enum State {
    Closed,
    Open(Instant),
    HalfOpen,
}

impl CircuitBreaker {
    async fn call<F, T>(&self, f: F) -> Result<T, Box<dyn Error>>
    where
        F: Future<Output = Result<T, Box<dyn Error>>>,
    {
        let state = self.state.lock().await;
        match *state {
            State::Open(opened_at) if opened_at.elapsed() < Duration::from_secs(60) => {
                Err("Circuit breaker is open".into())
            }
            _ => {
                drop(state);
                match f.await {
                    Ok(result) => {
                        self.failures.store(0, Ordering::SeqCst);
                        Ok(result)
                    }
                    Err(e) => {
                        let failures = self.failures.fetch_add(1, Ordering::SeqCst) + 1;
                        if failures >= self.threshold {
                            *self.state.lock().await = State::Open(Instant::now());
                        }
                        Err(e)
                    }
                }
            }
        }
    }
}
```

---

## Migration from libzmq

### API Differences

| libzmq (C) | libzmq (Python) | Monocoque (Rust) |
|------------|-----------------|------------------|
| `zmq_socket(ctx, ZMQ_REQ)` | `ctx.socket(zmq.REQ)` | `ReqSocket::new(stream)` |
| `zmq_connect(sock, addr)` | `sock.connect(addr)` | `TcpStream::connect(addr)` |
| `zmq_send(sock, data, len, 0)` | `sock.send(data)` | `socket.send(vec![data])` |
| `zmq_recv(sock, buf, len, 0)` | `sock.recv()` | `socket.recv().await` |

### Migration Strategy

**Phase 1: Side-by-Side Deployment**
- Deploy Monocoque services alongside libzmq services
- Route subset of traffic to Monocoque
- Monitor metrics and compare

**Phase 2: Gradual Rollout**
- Increase Monocoque traffic to 10%, 25%, 50%, 75%
- Validate no regressions at each step
- Keep libzmq as fallback

**Phase 3: Full Migration**
- Route 100% traffic to Monocoque
- Deprecate libzmq services
- Remove libzmq dependencies

### Compatibility Testing

Run interop tests before migration:
```bash
cd interop_tests
pytest -v
```

---

## Deployment Patterns

### Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: monocoque-service
spec:
  replicas: 3
  template:
    spec:
      containers:
      - name: service
        image: myapp:latest
        env:
        - name: CURVE_SECRET_KEY
          valueFrom:
            secretKeyRef:
              name: curve-keys
              key: secret
        - name: RUST_LOG
          value: "info,monocoque_zmtp=debug"
        resources:
          requests:
            memory: "256Mi"
            cpu: "500m"
          limits:
            memory: "512Mi"
            cpu: "1000m"
        livenessProbe:
          tcpSocket:
            port: 5555
          initialDelaySeconds: 10
          periodSeconds: 30
```

### Docker Compose

```yaml
version: '3.8'
services:
  rep-server:
    build: .
    environment:
      - RUST_LOG=info
      - CURVE_SECRET_KEY=${CURVE_SECRET_KEY}
    ports:
      - "5555:5555"
    restart: unless-stopped
    
  req-client:
    build: .
    environment:
      - SERVER_PUBLIC_KEY=${SERVER_PUBLIC_KEY}
    depends_on:
      - rep-server
    restart: unless-stopped
```

---

## Troubleshooting

### Connection Timeouts

**Symptom:** Connections hang or timeout

**Debug:**
```bash
# Check network connectivity
nc -vz server_ip 5555

# Check firewall
sudo iptables -L -n | grep 5555

# Enable debug logging
export RUST_LOG=monocoque_zmtp=debug
```

**Solutions:**
- Increase handshake timeout
- Check firewall rules
- Verify server is listening (netstat/ss)

### Authentication Failures

**Symptom:** "Authentication failed" errors

**Debug:**
```bash
# Check ZAP server is running
ps aux | grep zap

# Verify credentials
echo $CURVE_SECRET_KEY | wc -c  # Should be 64 hex chars

# Check logs
grep "ZAP" application.log
```

**Solutions:**
- Verify credentials match exactly
- Check ZAP handler is running
- Increase ZAP timeout

### Performance Issues

**Symptom:** Low throughput or high latency

**Debug:**
```bash
# Profile with perf
perf record -g cargo run --release
perf report

# Check buffer sizes
RUST_LOG=monocoque_zmtp=trace cargo run 2>&1 | grep buffer
```

**Solutions:**
- Tune buffer sizes for message sizes
- Enable TCP_NODELAY for latency
- Use batching API for throughput

---

## Production Checklist Summary

### Before Go-Live
- [ ] Security audit complete
- [ ] Load testing passed
- [ ] Monitoring deployed
- [ ] Rollback plan tested
- [ ] Documentation complete
- [ ] Team trained

### Launch Day
- [ ] Enable gradual rollout (10%)
- [ ] Monitor metrics closely
- [ ] Check error rates
- [ ] Verify authentication working
- [ ] Test rollback procedure

### Post-Launch
- [ ] Review metrics daily (week 1)
- [ ] Gradual traffic increase
- [ ] Document lessons learned
- [ ] Update runbooks
- [ ] Plan optimization work

---

## Support & Resources

- **Documentation:** https://docs.monocoque.rs
- **Issue Tracker:** https://github.com/monocoque/issues
- **Security:** security@monocoque.rs
- **Community:** Discord/Slack channel

