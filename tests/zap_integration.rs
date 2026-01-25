//! ZAP (ZeroMQ Authentication Protocol) Integration Tests
//!
//! Tests complete ZAP workflow with PLAIN and CURVE mechanisms.

use monocoque_zmtp::security::plain::{plain_server_handshake_zap, PlainClientHandshake, StaticPlainHandler};
use monocoque_zmtp::security::zap_handler::{DefaultZapHandler, spawn_zap_server};
use monocoque_zmtp::TcpSocket;
use std::io;
use std::sync::Arc;
use std::time::Duration;

/// Test PLAIN authentication via ZAP - successful authentication
#[compio::test]
async fn test_plain_zap_success() -> io::Result<()> {
    // Start ZAP server with credentials
    let mut handler = StaticPlainHandler::new();
    handler.add_user("testuser", "testpass");
    let plain_handler = Arc::new(handler);
    let zap_handler = Arc::new(DefaultZapHandler::new(plain_handler, false));
    spawn_zap_server(zap_handler)?;

    // Give ZAP server time to bind
    compio::time::sleep(Duration::from_millis(100)).await;

    // Create server socket listening on localhost
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    // Spawn server task
    let server_task = compio::runtime::spawn(async move {
        let (stream, _peer_addr) = listener.accept().await?;
        
        // Perform ZAP-based PLAIN handshake
        let _socket_base = plain_server_handshake_zap::<compio::net::TcpStream>(
            stream,
            "testdomain".to_string(),
            None,
        )
        .await?;
        
        Ok::<(), io::Error>(())
    });

    // Give server time to start
    compio::time::sleep(Duration::from_millis(50)).await;

    // Create client and connect
    let client_stream = compio::net::TcpStream::connect(server_addr).await?;
    
    // Perform client handshake with valid credentials
    let client_handshake = PlainClientHandshake::new("testuser", "testpass");
    let _client_socket = TcpSocket::connect_with_handshake(
        client_stream,
        "monocoque_test",
        client_handshake,
    )
    .await?;

    // Wait for server to complete
    server_task.await?;

    Ok(())
}

/// Test PLAIN authentication via ZAP - failed authentication
#[compio::test]
async fn test_plain_zap_failure() -> io::Result<()> {
    // ZAP server from previous test should still be running
    compio::time::sleep(Duration::from_millis(100)).await;

    // Create server socket
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    // Spawn server task
    let server_task = compio::runtime::spawn(async move {
        let (stream, _peer_addr) = listener.accept().await?;
        
        // Perform ZAP-based PLAIN handshake - should fail
        let result = plain_server_handshake_zap::<compio::net::TcpStream>(
            stream,
            "testdomain".to_string(),
            None,
        )
        .await;
        
        // Server should reject the connection
        assert!(result.is_err(), "Expected authentication to fail");
        
        Ok::<(), io::Error>(())
    });

    // Give server time to start
    compio::time::sleep(Duration::from_millis(50)).await;

    // Create client with WRONG credentials
    let client_stream = compio::net::TcpStream::connect(server_addr).await?;
    
    let client_handshake = PlainClientHandshake::new("testuser", "WRONGPASS");
    let result = TcpSocket::connect_with_handshake(
        client_stream,
        "monocoque_test",
        client_handshake,
    )
    .await;

    // Client should fail to connect
    assert!(result.is_err(), "Expected client authentication to fail");

    // Wait for server
    let _ = server_task.await;

    Ok(())
}

/// Test ZAP timeout handling
#[compio::test]
async fn test_zap_timeout() -> io::Result<()> {
    // Create server without ZAP server running
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    // Spawn server task
    let server_task = compio::runtime::spawn(async move {
        let (stream, _peer_addr) = listener.accept().await?;
        
        // This should timeout because no ZAP server is handling requests
        let result = plain_server_handshake_zap::<compio::net::TcpStream>(
            stream,
            "testdomain".to_string(),
            Some(Duration::from_millis(500)),
        )
        .await;
        
        // Should fail due to timeout
        assert!(result.is_err(), "Expected ZAP timeout");
        
        Ok::<(), io::Error>(())
    });

    // Give server time to start
    compio::time::sleep(Duration::from_millis(50)).await;

    // Create client
    let client_stream = compio::net::TcpStream::connect(server_addr).await?;
    
    let client_handshake = PlainClientHandshake::new("testuser", "testpass");
    let result = TcpSocket::connect_with_handshake(
        client_stream,
        "monocoque_test",
        client_handshake,
    )
    .await;

    // Should fail
    assert!(result.is_err(), "Expected connection to fail due to ZAP timeout");

    // Wait for server
    let _ = server_task.await;

    Ok(())
}
