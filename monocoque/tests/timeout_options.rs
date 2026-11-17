//! Timeout options whose documented meaning the code did not match.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, Msg, SocketOptions};
use std::time::Duration;

/// `handshake_timeout` of zero means no limit. It used to be passed on as a
/// budget of zero, which failed every handshake at its first read.
#[test]
fn a_zero_handshake_timeout_means_no_limit() {
    LocalRuntime::new().unwrap().block_on(async {
        let opts = SocketOptions::default()
            .with_handshake_timeout(Duration::ZERO)
            .with_recv_timeout(Duration::from_secs(5));
        let mut server = DealerSocket::with_options(opts.clone());
        let ep = server.bind("tcp://127.0.0.1:0").await.unwrap();
        let mut client = DealerSocket::with_options(opts);
        client.connect(&ep).await.unwrap();

        client.send(Msg::from("hello")).await.unwrap();
        assert_eq!(server.recv().await.unwrap(), [b"hello"]);
    });
}
