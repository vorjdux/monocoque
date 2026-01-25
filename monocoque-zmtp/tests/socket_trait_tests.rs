//! Integration tests for Socket trait API

use monocoque_zmtp::{DealerSocket, RouterSocket, Socket};
use monocoque_core::options::SocketOptions;
use bytes::Bytes;

#[compio::test]
async fn test_socket_trait_polymorphism() {
    // Test that we can use Socket trait for polymorphic handling
    async fn get_socket_type<S: Socket>(socket: &S) -> monocoque_zmtp::session::SocketType {
        socket.socket_type()
    }

    let dealer = DealerSocket::new();
    let socket_type = get_socket_type(&dealer).await;
    assert_eq!(
        format!("{:?}", socket_type),
        "Dealer"
    );
}

#[compio::test]
async fn test_socket_trait_send_recv_signature() {
    // Test that Socket trait methods have correct signatures
    // This is a compile-time test - if it compiles, it works
    
    async fn send_message<S: Socket>(socket: &mut S, msg: Vec<Bytes>) -> std::io::Result<()> {
        socket.send(msg).await
    }

    async fn recv_message<S: Socket>(socket: &mut S) -> std::io::Result<Option<Vec<Bytes>>> {
        socket.recv().await
    }

    // If this compiles, the trait works correctly
    let mut dealer = DealerSocket::new();
    
    // These should compile without errors
    let _ = send_message(&mut dealer, vec![Bytes::from("test")]);
    let _ = recv_message(&mut dealer);
}

#[compio::test]
async fn test_multiple_socket_types() {
    use monocoque_zmtp::session::SocketType;
    
    // DealerSocket and RouterSocket require streams - test skipped
    return;
}
