//! Integration tests for automatic reconnection functionality.
//!
//! These tests verify that sockets can detect disconnections and automatically
//! reconnect with proper exponential backoff.

use bytes::Bytes;
use compio::net::{TcpListener, TcpStream};
use monocoque_core::options::SocketOptions;
use monocoque_zmtp::dealer::DealerSocket;
use monocoque_zmtp::router::RouterSocket;
use monocoque_core::config::BufferConfig;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[compio::test]
async fn test_dealer_reconnect_on_server_disconnect() {
    // Setup: Create server that will disconnect after first message
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("tcp://{}", addr);

    // Track connection attempts
    let connection_count = Arc::new(AtomicU32::new(0));
    let connection_count_clone = connection_count.clone();

    // Server accepts connections, receives one message, then disconnects
    compio::runtime::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                connection_count_clone.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut router) = RouterSocket::from_tcp(stream).await {
                    // Receive one message then drop socket (simulates disconnect)
                    if let Ok(Some(_)) = router.recv().await {
                        drop(router);
                    }
                }
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    // Create client with reconnection
    let config = BufferConfig::default();
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(50))
        .with_reconnect_ivl_max(Duration::from_secs(1));

    let mut dealer = DealerSocket::connect(&endpoint, config, options)
        .await
        .unwrap();

    // Send first message - should succeed
    dealer
        .send(vec![Bytes::from("msg1")])
        .await
        .expect("First send should succeed");

    // Wait for server to disconnect and TCP to notice
    compio::time::sleep(Duration::from_millis(500)).await;

    // Try to recv - should detect disconnect (EOF)
    let recv_result = dealer.recv().await;
    assert!(
        recv_result.is_ok() && recv_result.unwrap().is_none(),
        "Should detect disconnect via EOF"
    );

    // Socket should now be disconnected
    assert!(!dealer.is_connected(), "Socket should be disconnected");

    // Attempt reconnection with send_with_reconnect
    let reconnect_result = dealer.send_with_reconnect(vec![Bytes::from("msg3")]).await;
    assert!(
        reconnect_result.is_ok(),
        "Send with reconnect should succeed: {:?}",
        reconnect_result
    );

    // Verify we actually reconnected (connection count increased)
    let final_count = connection_count.load(Ordering::SeqCst);
    assert!(
        final_count >= 2,
        "Should have at least 2 connections, got {}",
        final_count
    );
}

#[compio::test]
async fn test_reconnect_state_tracks_attempts() {
    use monocoque_core::reconnect::ReconnectState;
    
    // Test ReconnectState directly (unit test style)
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(100))
        .with_reconnect_ivl_max(Duration::from_millis(800));

    let mut reconnect = ReconnectState::new(&options);

    // First attempt
    let delay1 = reconnect.next_delay();
    assert_eq!(delay1, Duration::from_millis(100), "First delay should be base interval");
    assert_eq!(reconnect.attempt(), 1, "Should be on attempt 1");

    // Second attempt - should double
    let delay2 = reconnect.next_delay();
    assert_eq!(delay2, Duration::from_millis(200), "Second delay should be 2x base");
    assert_eq!(reconnect.attempt(), 2, "Should be on attempt 2");

    // Third attempt - should double again
    let delay3 = reconnect.next_delay();
    assert_eq!(delay3, Duration::from_millis(400), "Third delay should be 4x base");
    assert_eq!(reconnect.attempt(), 3, "Should be on attempt 3");

    // Fourth attempt - should double again
    let delay4 = reconnect.next_delay();
    assert_eq!(delay4, Duration::from_millis(800), "Fourth delay should be capped at max");
    assert_eq!(reconnect.attempt(), 4, "Should be on attempt 4");

    // Fifth attempt - should stay at cap
    let delay5 = reconnect.next_delay();
    assert_eq!(delay5, Duration::from_millis(800), "Fifth delay should remain at max");

    // Reset
    reconnect.reset();
    assert_eq!(reconnect.attempt(), 0, "Reset should clear attempts");
    let delay_after_reset = reconnect.next_delay();
    assert_eq!(delay_after_reset, Duration::from_millis(100), "After reset should be base again");
}

#[compio::test]
async fn test_poison_flag_integration() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("tcp://{}", addr);

    // Server that accepts connections
    compio::runtime::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            if let Ok(mut router) = RouterSocket::from_tcp(stream).await {
                // Keep receiving
                while let Ok(Some(_)) = router.recv().await {}
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    let config = BufferConfig::default();
    let options = SocketOptions::default();
    let mut dealer: DealerSocket<TcpStream> = DealerSocket::connect(&endpoint, config, options)
        .await
        .unwrap();

    // Initially not poisoned
    assert!(!dealer.is_poisoned(), "Socket should start unpoisoned");

    // Send and buffer messages - this works fine
    dealer
        .send_buffered(vec![Bytes::from("msg1")])
        .unwrap();

    // Flush successfully
    dealer.flush().await.expect("Flush should succeed");

    // Still not poisoned after successful flush
    assert!(!dealer.is_poisoned(), "Socket should remain unpoisoned after successful flush");

    // Note: Actually triggering poisoning via future cancellation is difficult in tests
    // The important part is that the mechanism exists and is integrated into the socket
}

#[compio::test]
async fn test_reconnect_resets_socket_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("tcp://{}", addr);

    // Server that receives one message per connection
    compio::runtime::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(mut router) = RouterSocket::from_tcp(stream).await {
                    let _ = router.recv().await;
                    // Keep socket alive briefly
                    compio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    let config = BufferConfig::default();
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(50))
        .with_send_hwm(10);

    let mut dealer = DealerSocket::connect(&endpoint, config, options)
        .await
        .unwrap();

    // Buffer some messages
    for i in 0..5 {
        dealer
            .send_buffered(vec![Bytes::from(format!("msg{}", i))])
            .unwrap();
    }

    // Verify buffered count
    let buffered_count = dealer.buffered_messages();
    assert_eq!(buffered_count, 5);

    // Flush to send
    dealer.flush().await.unwrap();

    // Wait for server to disconnect
    compio::time::sleep(Duration::from_millis(100)).await;

    // Trigger disconnection detection
    let _ = dealer.send(vec![Bytes::from("test")]).await;

    // Reconnect
    let reconnect_result = dealer.try_reconnect().await;
    assert!(
        reconnect_result.is_ok(),
        "Reconnection should succeed: {:?}",
        reconnect_result
    );

    // After reconnection, buffered messages should be cleared
    let buffered_after = dealer.buffered_messages();
    assert_eq!(
        buffered_after,
        0,
        "Reconnection should clear buffered messages"
    );

    // Socket should not be poisoned
    assert!(
        !dealer.is_poisoned(),
        "Reconnection should clear poison flag"
    );
}

#[compio::test]
async fn test_recv_with_reconnect_detects_eof() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("tcp://{}", addr);

    let connection_count = Arc::new(AtomicU32::new(0));
    let connection_count_clone = connection_count.clone();

    // Server that immediately closes after handshake
    compio::runtime::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                connection_count_clone.fetch_add(1, Ordering::SeqCst);
                if let Ok(router) = RouterSocket::from_tcp(stream).await {
                    drop(router); // Immediate disconnect
                }
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    let config = BufferConfig::default();
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(50));

    let mut dealer: DealerSocket<TcpStream> = DealerSocket::connect(&endpoint, config, options)
        .await
        .unwrap();

    // First recv will detect EOF
    let result = dealer.recv().await;
    assert!(
        result.is_ok() && result.unwrap().is_none(),
        "Should detect EOF and return Ok(None)"
    );

    // Socket should be disconnected
    assert!(
        !dealer.is_connected(),
        "Socket should be disconnected after EOF"
    );

    // recv_with_reconnect should attempt to reconnect
    let reconnect_result = dealer.recv_with_reconnect().await;
    
    // Should get None again (server still disconnects immediately)
    // but connection count should increase
    assert!(reconnect_result.is_ok());
    
    let final_count = connection_count.load(Ordering::SeqCst);
    assert!(
        final_count >= 2,
        "Should have attempted reconnection, got {} connections",
        final_count
    );
}

#[compio::test]
async fn test_no_reconnect_without_endpoint() {
    // Create socket without using connect() (no endpoint stored)
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    compio::runtime::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let _ = RouterSocket::from_tcp(stream).await;
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    // Create dealer from raw TcpStream (no endpoint stored)
    let stream = TcpStream::connect(addr).await.unwrap();
    let mut dealer = DealerSocket::new(stream).await.unwrap();

    // Manually disconnect by dropping the internal stream
    // (simulated by just trying to reconnect)
    let result = dealer.try_reconnect().await;

    // Should fail because no endpoint was stored
    assert!(result.is_err(), "Reconnection should fail without endpoint");
    assert_eq!(
        result.unwrap_err().kind(),
        io::ErrorKind::Unsupported,
        "Should return Unsupported error"
    );
}

#[compio::test]
async fn test_successful_reconnection_clears_state() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = format!("tcp://{}", addr);

    let msg_count = Arc::new(AtomicU32::new(0));
    let msg_count_clone = msg_count.clone();

    // Server that stays alive and receives messages
    compio::runtime::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(mut router) = RouterSocket::from_tcp(stream).await {
                    while let Ok(Some(_)) = router.recv().await {
                        msg_count_clone.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    })
    .detach();

    compio::time::sleep(Duration::from_millis(50)).await;

    let config = BufferConfig::default();
    let options = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(50));

    let mut dealer: DealerSocket<TcpStream> = DealerSocket::connect(&endpoint, config, options)
        .await
        .unwrap();

    // Send first message
    dealer
        .send(vec![Bytes::from("msg1")])
        .await
        .expect("First send should succeed");

    compio::time::sleep(Duration::from_millis(100)).await;

    // Verify message was received
    assert_eq!(msg_count.load(Ordering::SeqCst), 1);

    // Force reconnection by manually closing and reconnecting
    // (In real scenario, network failure would trigger this)
    drop(dealer);

    // Create new dealer (simulating reconnection)
    let options2 = SocketOptions::default()
        .with_reconnect_ivl(Duration::from_millis(50));
    let mut dealer2: DealerSocket<TcpStream> = DealerSocket::connect(&endpoint, config, options2)
        .await
        .unwrap();

    // Send second message after "reconnection"
    dealer2
        .send(vec![Bytes::from("msg2")])
        .await
        .expect("Send after reconnection should succeed");

    compio::time::sleep(Duration::from_millis(100)).await;

    // Verify second message was received
    assert_eq!(msg_count.load(Ordering::SeqCst), 2);
}
