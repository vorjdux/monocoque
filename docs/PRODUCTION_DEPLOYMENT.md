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
// Default (32 KB read) suits most workloads
let options = SocketOptions::default();

// Small messages (<1 KB), trading throughput for latency
let options = SocketOptions::default().with_read_buffer_size(4096);

// Bulk transfers: widen the read batch to the full 64 KB slab
let options = SocketOptions::default().with_read_buffer_size(65_536);
```

The read buffer is clamped to the 64 KB read slab and floored at 64 bytes. There is no write buffer setting: the outbound buffer grows on demand and is reused.

### TCP Options

`TCP_NODELAY` is always enabled, on every connection, so Nagle's algorithm never
delays a send. There is no option to turn it off: eager delivery is the contract,
and batching is opted into explicitly with write coalescing instead.

To detect dead peers, enable keepalive:

```rust
let options = SocketOptions::default()
    .with_tcp_keepalive(1)          // 1 enables, 0 disables, -1 leaves the OS default
    .with_tcp_keepalive_idle(60)    // seconds idle before probing
    .with_tcp_keepalive_intvl(10)   // seconds between probes
    .with_tcp_keepalive_cnt(3);     // failed probes before the connection is dropped
```

For high-throughput pub/sub, batch in userspace and give the kernel a larger
send buffer:

```rust
let options = SocketOptions::default()
    .with_write_coalescing(true)
    .with_sndbuf(128 * 1024);
```

For connections over WAN (multi-region), extend timeouts:

```rust
options.handshake_timeout = Duration::from_secs(30);
options.connect_timeout = Duration::from_secs(30);
options.tcp_keepalive_idle = 120;
```

## Error Handling

### Retry with Backoff

Connected endpoints redial on their own, with exponential backoff and jitter, so
there is no retry loop to write. Tune it with options:

```rust
let options = SocketOptions::default()
    .with_reconnect_ivl(Duration::from_millis(100))
    .with_reconnect_ivl_max(Duration::from_secs(10))
    .with_max_reconnect_attempts(None); // None retries indefinitely

let mut socket = ReqSocket::with_options(options);
socket.connect("tcp://server:5555").await?;
```

See [RELIABILITY_AND_RESILIENCE.md](RELIABILITY_AND_RESILIENCE.md) for details.

### Send/Receive Timeouts

Set timeouts on the socket rather than wrapping each call. An elapsed timeout
returns `MonocoqueError::TimedOut` and leaves the socket usable:

```rust
let options = SocketOptions::default()
    .with_send_timeout(Duration::from_secs(5))
    .with_recv_timeout(Duration::from_secs(5));

match socket.send(msg).await {
    Err(MonocoqueError::TimedOut) => { /* no peer had room in time */ }
    other => other?,
}
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

**Low throughput** - check buffer sizes match your message sizes, and consider `with_write_coalescing(true)` for small-message PUB/SUB bursts to batch writes.

**High CPU from CURVE** - this is expected at connection time; per-message overhead is small. If the handshake cost matters, pool connections.

## Pre-Launch Checklist

- [ ] `ulimit -n` set to 65536 or higher in the service unit
- [ ] Kernel network buffers tuned (`sysctl net.core.rmem_max`)
- [ ] Buffer sizes match typical message size
- [ ] TCP keepalive configured (especially for long-idle connections)
- [ ] Reconnect interval and attempt limit reviewed for all connection sites
- [ ] Metrics collection wired up
- [ ] `RUST_LOG` set appropriately (not `trace` in production)
- [ ] Load tested under realistic message rates
- [ ] Rollback procedure documented and tested
- [ ] Security configured per [SECURITY_GUIDE.md](SECURITY_GUIDE.md)
