//! REQ Socket State Machine Integration Tests
//!
//! Tests strict and relaxed modes for REQ socket enforcement.

use monocoque_zmtp::req::ReqSocket;
use monocoque_zmtp::rep::RepSocket;
use monocoque_core::options::SocketOptions;
use bytes::Bytes;
use std::io;

/// Test strict REQ state machine - send→send should fail
#[compio::test]
async fn test_req_strict_send_send_fails() -> io::Result<()> {
    // Setup REP server
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut rep_socket = RepSocket::new(stream).await?;
        
        // Receive first request
        let _req = rep_socket.recv().await?;
        rep_socket.send(vec![Bytes::from("reply1")]).await?;
        
        Ok::<(), io::Error>(())
    });

    // Give server time to start
    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create REQ socket with STRICT mode (req_relaxed = false, which is default)
    let stream = compio::net::TcpStream::connect(server_addr).await?;
    let mut options = SocketOptions::default();
    options.req_relaxed = false; // Explicit strict mode
    let mut req_socket = ReqSocket::with_options(stream, options).await?;

    // First send should work
    req_socket.send(vec![Bytes::from("request1")]).await?;

    // Second send WITHOUT recv should FAIL in strict mode
    let result = req_socket.send(vec![Bytes::from("request2")]).await;
    assert!(result.is_err(), "Expected error when sending twice without recv in strict mode");
    
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("await reply") || err.to_string().contains("recv"));

    // Clean up
    let _reply = req_socket.recv().await?;
    server_task.await?;

    Ok(())
}

/// Test strict REQ state machine - recv→recv should fail
#[compio::test]
async fn test_req_strict_recv_recv_fails() -> io::Result<()> {
    // Setup REP server
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut rep_socket = RepSocket::new(stream).await?;
        
        // Receive request and send reply
        let _req = rep_socket.recv().await?;
        rep_socket.send(vec![Bytes::from("reply1")]).await?;
        
        Ok::<(), io::Error>(())
    });

    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create REQ socket in strict mode
    let stream = compio::net::TcpStream::connect(server_addr).await?;
    let mut req_socket = ReqSocket::new(stream).await?; // Default is strict

    // Send and recv - normal flow
    req_socket.send(vec![Bytes::from("request1")]).await?;
    let _reply = req_socket.recv().await?;

    // Now socket is in Idle state - recv without send should FAIL
    let result = req_socket.recv().await;
    assert!(result.is_err(), "Expected error when receiving twice without send in strict mode");
    
    let err = result.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("Idle") || err.to_string().contains("send"));

    server_task.await?;
    Ok(())
}

/// Test relaxed REQ mode - send→send should succeed
#[compio::test]
async fn test_req_relaxed_send_send_succeeds() -> io::Result<()> {
    // Setup REP server that handles multiple requests
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut rep_socket = RepSocket::new(stream).await?;
        
        // Handle first request
        let _req1 = rep_socket.recv().await?;
        rep_socket.send(vec![Bytes::from("reply1")]).await?;
        
        // Handle second request
        let _req2 = rep_socket.recv().await?;
        rep_socket.send(vec![Bytes::from("reply2")]).await?;
        
        Ok::<(), io::Error>(())
    });

    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create REQ socket with RELAXED mode
    let stream = compio::net::TcpStream::connect(server_addr).await?;
    let mut options = SocketOptions::default();
    options.req_relaxed = true; // Enable relaxed mode
    let mut req_socket = ReqSocket::with_options(stream, options).await?;

    // Send MULTIPLE requests without waiting for replies (relaxed mode)
    req_socket.send(vec![Bytes::from("request1")]).await?;
    req_socket.send(vec![Bytes::from("request2")]).await?; // Should succeed in relaxed mode

    // Now receive both replies
    let reply1 = req_socket.recv().await?;
    assert!(reply1.is_some());
    
    let reply2 = req_socket.recv().await?;
    assert!(reply2.is_some());

    server_task.await?;
    Ok(())
}

/// Test strict REQ mode - normal alternating send/recv works
#[compio::test]
async fn test_req_strict_normal_flow() -> io::Result<()> {
    // Setup REP server
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut rep_socket = RepSocket::new(stream).await?;
        
        // Handle 3 request-reply cycles
        for i in 0..3 {
            let req = rep_socket.recv().await?.expect("Should receive request");
            assert_eq!(req[0], Bytes::from(format!("request{}", i)));
            
            rep_socket.send(vec![Bytes::from(format!("reply{}", i))]).await?;
        }
        
        Ok::<(), io::Error>(())
    });

    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create REQ socket in strict mode
    let stream = compio::net::TcpStream::connect(server_addr).await?;
    let mut req_socket = ReqSocket::new(stream).await?;

    // Proper alternating send→recv→send→recv should work
    for i in 0..3 {
        req_socket.send(vec![Bytes::from(format!("request{}", i))]).await?;
        let reply = req_socket.recv().await?.expect("Should receive reply");
        assert_eq!(reply[0], Bytes::from(format!("reply{}", i)));
    }

    server_task.await?;
    Ok(())
}

/// Test REQ correlation mode - request IDs are validated
#[compio::test]
async fn test_req_correlation_mode() -> io::Result<()> {
    // Setup REP server that echoes back correlation IDs
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await?;
    let server_addr = listener.local_addr()?;

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut rep_socket = RepSocket::new(stream).await?;
        
        // Receive request (with correlation ID as first frame)
        let req = rep_socket.recv().await?.expect("Should receive");
        
        // Echo back the correlation ID + payload
        rep_socket.send(req).await?;
        
        Ok::<(), io::Error>(())
    });

    compio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create REQ socket with correlation enabled
    let stream = compio::net::TcpStream::connect(server_addr).await?;
    let mut options = SocketOptions::default();
    options.req_correlate = true; // Enable correlation
    let mut req_socket = ReqSocket::with_options(stream, options).await?;

    // Send request - correlation ID will be prepended automatically
    req_socket.send(vec![Bytes::from("payload")]).await?;
    
    // Receive reply - correlation ID will be validated and stripped
    let reply = req_socket.recv().await?.expect("Should receive");
    
    // Should only contain the payload (correlation ID stripped)
    assert_eq!(reply.len(), 1);
    assert_eq!(reply[0], Bytes::from("payload"));

    server_task.await?;
    Ok(())
}
