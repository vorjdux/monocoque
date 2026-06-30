# Production Deployment

This covers OS-level tuning, socket configuration, monitoring, and a short pre-launch checklist. For security configuration (CURVE, PLAIN, ZAP), see [SECURITY_GUIDE.md](SECURITY_GUIDE.md).

## OS Configuration

### File Descriptors

Each open connection consumes a file descriptor. The default limit (1024) is too low for anything serious.

```bash
# /etc/security/limits.conf
* soft nofile 65536
* hard nofile 65536

# Or for the process at launch:
ulimit -n 65536
```

In systemd units:

```ini
[Service]
LimitNOFILE=65536
```

### Network Buffers

The kernel's default TCP send/receive buffers cap throughput on high-latency links.

```bash
# /etc/sysctl.conf or sysctl.d/
net.core.rmem_max = 134217728      # 128 MB
net.core.wmem_max = 134217728
net.ipv4.tcp_rmem = 4096 87380 134217728
net.ipv4.tcp_wmem = 4096 65536 134217728
net.core.netdev_max_backlog = 5000
```

Apply without reboot: `sysctl -p`

## Socket Tuning

### Buffer Sizing

Match buffer sizes to your typical message size:

```rust
// Small messages (<1 KB)
let options = SocketOptions::small();   // 4 KB buffers

// Default (1-10 KB messages)
let options = SocketOptions::default(); // 8 KB buffers

// Large messages (>10 KB)
let options = SocketOptions::large();   // 16 KB buffers

// Custom
let mut options = SocketOptions::default();
options.recv_buffer_size = 32 * 1024;
options.send_buffer_size = 32 * 1024;
```

### TCP Options

For low-latency request/reply, disable Nagle's algorithm:

```rust
options.tcp_nodelay = true;
options.tcp_keepalive = true;
options.tcp_keepalive_idle = 60;      // seconds idle before probing
options.tcp_keepalive_interval = 10;  // seconds between probes
```

For high-throughput pub/sub, let the kernel batch:

```rust
options.tcp_nodelay = false;
options.send_buffer_size = 128 * 1024;
```

For connections over WAN (multi-region), extend timeouts:

```rust
options.handshake_timeout = Duration::from_secs(30);
options.connect_timeout = Duration::from_secs(30);
options.tcp_keepalive_idle = 120;
```

## Error Handling

### Retry with Backoff

```rust
use backoff::{ExponentialBackoff, backoff::Backoff};

async fn connect_with_retry(addr: &str) -> io::Result<ReqSocket> {
    let mut backoff = ExponentialBackoff::default();
    loop {
        match TcpStream::connect(addr).await {
            Ok(stream) => return ReqSocket::new(stream).await,
            Err(e) => match backoff.next_backoff() {
                Some(d) => tokio::time::sleep(d).await,
                None => return Err(e),
            },
        }
    }
}
```

### Send/Receive Timeouts

```rust
use tokio::time::timeout;

timeout(Duration::from_secs(5), socket.send(msg))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "send timeout"))??;
```

## Monitoring

Track these at minimum:

- Active connections and connection failure rate
- Messages sent/received per second
- Send/receive latency (p50, p95, p99)
- Authentication failures (if using CURVE or PLAIN)

Prometheus example:

```rust
use prometheus::{register_counter, register_histogram};

lazy_static! {
    static ref MESSAGES_SENT: Counter =
        register_counter!("monocoque_messages_sent_total", "Total messages sent").unwrap();
    static ref SEND_LATENCY: Histogram =
        register_histogram!("monocoque_send_latency_seconds", "Send latency").unwrap();
}

let start = Instant::now();
socket.send(msg).await?;
MESSAGES_SENT.inc();
SEND_LATENCY.observe(start.elapsed().as_secs_f64());
```

Set `RUST_LOG=info,monocoque_zmtp=debug` during rollout. Drop to `info` once stable.

## Troubleshooting

**Connections hang or time out** - check firewall rules (`ss -tlnp`, `iptables -L`), verify the server is listening, and increase `handshake_timeout` for slow networks.

**Low throughput** - check buffer sizes match your message sizes, consider disabling `tcp_nodelay` for PUB/SUB to allow batching.

**High CPU from CURVE** - this is expected at connection time; per-message overhead is small. If the handshake cost matters, pool connections.

## Pre-Launch Checklist

- [ ] `ulimit -n` set to 65536 or higher in the service unit
- [ ] Kernel network buffers tuned (`sysctl net.core.rmem_max`)
- [ ] Buffer sizes match typical message size
- [ ] TCP keepalive configured (especially for long-idle connections)
- [ ] Retry/backoff logic in place for all connection sites
- [ ] Metrics collection wired up
- [ ] `RUST_LOG` set appropriately (not `trace` in production)
- [ ] Load tested under realistic message rates
- [ ] Rollback procedure documented and tested
- [ ] Security configured per [SECURITY_GUIDE.md](SECURITY_GUIDE.md)
