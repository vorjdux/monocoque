//! Integration tests for Socket trait API

use bytes::Bytes;
use monocoque_zmtp::Socket;

#[compio::test]
async fn test_socket_trait_send_recv_signature() {
    // Verify the Socket trait's send/recv method signatures compile correctly.
    // This test is primarily a compile-time check.
    async fn send_message<S: Socket>(socket: &mut S, msg: Vec<Bytes>) -> std::io::Result<()> {
        socket.send(msg).await
    }

    async fn recv_message<S: Socket>(socket: &mut S) -> std::io::Result<Option<Vec<Bytes>>> {
        socket.recv().await
    }

    // Verify socket_type via trait
    fn check_type<S: Socket>(socket: &S) -> monocoque_zmtp::session::SocketType {
        socket.socket_type()
    }

    // Test that the trait is implemented correctly by using real connected sockets
    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        monocoque_zmtp::RouterSocket::from_tcp(stream)
            .await
            .unwrap()
    });

    let mut dealer = monocoque_zmtp::DealerSocket::connect(addr).await.unwrap();
    let mut router = server_task.await;

    // Verify types via trait
    assert_eq!(
        check_type(&dealer),
        monocoque_zmtp::session::SocketType::Dealer
    );
    assert_eq!(
        check_type(&router),
        monocoque_zmtp::session::SocketType::Router
    );

    // Verify send/recv compile with the trait
    send_message(&mut dealer, vec![Bytes::from("hello")])
        .await
        .unwrap();
    let msg = recv_message(&mut router).await.unwrap();
    assert!(msg.is_some());
}

#[compio::test]
async fn test_multiple_socket_types() {
    use monocoque_zmtp::session::SocketType;

    let listener = compio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = compio::runtime::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        monocoque_zmtp::RouterSocket::from_tcp(stream)
            .await
            .unwrap()
    });

    let dealer = monocoque_zmtp::DealerSocket::connect(addr).await.unwrap();
    let router = server_task.await;

    assert_eq!(dealer.socket_type(), SocketType::Dealer);
    assert_eq!(router.socket_type(), SocketType::Router);
}
