//! The network tuning options: their defaults and their builders.

use monocoque_core::options::SocketOptions;
use std::time::Duration;

#[test]
fn test_network_tuning_options() {
    let opts = SocketOptions::new()
        .with_sndbuf(65536)
        .with_rcvbuf(65536)
        .with_tos(0x10)
        .with_connect_timeout(Duration::from_secs(3));

    assert_eq!(opts.sndbuf, 65536);
    assert_eq!(opts.rcvbuf, 65536);
    assert_eq!(opts.tos, 0x10);
    assert_eq!(opts.connect_timeout, Duration::from_secs(3));
}

#[test]
fn test_default_network_values() {
    let opts = SocketOptions::default();

    assert_eq!(opts.sndbuf, 0); // OS default
    assert_eq!(opts.rcvbuf, 0); // OS default
    assert_eq!(opts.tos, 0); // OS default
    assert_eq!(opts.connect_timeout, Duration::ZERO); // OS default
}

#[test]
fn test_combined_options() {
    let opts = SocketOptions::new()
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5))
        .with_tcp_keepalive(1)
        .with_sndbuf(131_072);

    assert_eq!(opts.recv_timeout, Some(Duration::from_secs(5)));
    assert_eq!(opts.send_timeout, Some(Duration::from_secs(5)));
    assert_eq!(opts.tcp_keepalive, 1);
    assert_eq!(opts.sndbuf, 131_072);
}
