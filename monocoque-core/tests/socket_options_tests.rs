//! Integration tests for new socket options

use monocoque_core::options::SocketOptions;
use std::time::Duration;

#[test]
fn test_network_tuning_options() {
    let opts = SocketOptions::new()
        .with_rate(200)
        .with_recovery_ivl(Duration::from_secs(20))
        .with_sndbuf(65536)
        .with_rcvbuf(65536)
        .with_multicast_hops(5)
        .with_tos(0x10)
        .with_multicast_maxtpdu(9000);
    
    assert_eq!(opts.rate, 200);
    assert_eq!(opts.recovery_ivl, Duration::from_secs(20));
    assert_eq!(opts.sndbuf, 65536);
    assert_eq!(opts.rcvbuf, 65536);
    assert_eq!(opts.multicast_hops, 5);
    assert_eq!(opts.tos, 0x10);
    assert_eq!(opts.multicast_maxtpdu, 9000);
}

#[test]
fn test_ipv6_option() {
    let opts = SocketOptions::new().with_ipv6(true);
    assert!(opts.ipv6);
    
    let opts = SocketOptions::default();
    assert!(!opts.ipv6); // Default is false
}

#[test]
fn test_bind_to_device_option() {
    let opts = SocketOptions::new()
        .with_bind_to_device("eth0");
    
    assert_eq!(opts.bind_to_device, Some("eth0".to_string()));
    
    let opts = SocketOptions::default();
    assert_eq!(opts.bind_to_device, None);
}

#[test]
fn test_default_network_values() {
    let opts = SocketOptions::default();
    
    assert_eq!(opts.rate, 100); // 100 kbps
    assert_eq!(opts.recovery_ivl, Duration::from_secs(10));
    assert_eq!(opts.sndbuf, 0); // OS default
    assert_eq!(opts.rcvbuf, 0); // OS default
    assert_eq!(opts.multicast_hops, 1); // Local network only
    assert_eq!(opts.tos, 0); // Normal service
    assert_eq!(opts.multicast_maxtpdu, 1500); // Standard MTU
    assert!(!opts.ipv6);
    assert_eq!(opts.bind_to_device, None);
}

#[test]
fn test_combined_options() {
    // Test that new options work with existing ones
    let opts = SocketOptions::new()
        .with_recv_timeout(Duration::from_secs(5))
        .with_send_timeout(Duration::from_secs(5))
        .with_tcp_keepalive(1)
        .with_ipv6(true)
        .with_rate(500)
        .with_sndbuf(131072);
    
    assert_eq!(opts.recv_timeout, Some(Duration::from_secs(5)));
    assert_eq!(opts.send_timeout, Some(Duration::from_secs(5)));
    assert_eq!(opts.tcp_keepalive, 1);
    assert!(opts.ipv6);
    assert_eq!(opts.rate, 500);
    assert_eq!(opts.sndbuf, 131072);
}
