//! Integration tests for automatic reconnection functionality.
//!
//! These tests are placeholders for the reconnection feature, which requires
//! implementing a `DealerSocket::connect(endpoint, config, options)` API with
//! automatic backoff and reconnection support.
//!
//! Current status: reconnection is tracked via SocketOptions (reconnect_ivl,
//! reconnect_ivl_max) but not yet wired into the connection loop.

use bytes::Bytes;
use compio::net::TcpListener;
use monocoque_zmtp::dealer::DealerSocket;
use monocoque_zmtp::router::RouterSocket;
use std::time::Duration;

/// Test that a dealer socket can detect server disconnect and report EOF.
#[compio::test]
async fn test_dealer_detects_server_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut router = RouterSocket::from_tcp(stream).await.unwrap();
        let _ = router.recv().await;
        // Drop router - simulates server disconnect
    })
    .detach();

    compio::time::sleep(Duration::from_millis(20)).await;

    let mut dealer = DealerSocket::connect(addr).await.unwrap();
    dealer.send(vec![Bytes::from("test")]).await.unwrap();

    // After server drops connection, recv should return None (EOF)
    let result = dealer.recv().await;
    // Result can be Ok(None) or Err - either means connection closed
    match result {
        Ok(None) | Err(_) => {} // Expected: connection closed
        Ok(Some(_)) => panic!("Expected connection to be closed"),
    }
}

/// Automatic reconnection tests require the reconnection feature to be implemented.
/// The reconnection API (DealerSocket::connect_with_reconnect) is tracked in issue #10.
#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_dealer_reconnect_on_server_disconnect() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_reconnect_state_tracks_attempts() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_poison_flag_integration() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_reconnect_resets_socket_state() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_recv_with_reconnect_detects_eof() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_no_reconnect_without_endpoint() {}

#[compio::test]
#[ignore = "Reconnection feature not yet implemented - tracked in issue #10"]
async fn test_successful_reconnection_clears_state() {}
