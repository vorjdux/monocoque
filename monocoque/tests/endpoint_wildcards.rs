//! `*` in a TCP endpoint: somewhere to listen, never somewhere to dial.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{DealerSocket, MonocoqueError, SocketOptions};
use std::time::Duration;

fn opts() -> SocketOptions {
    SocketOptions::default().with_recv_timeout(Duration::from_secs(5))
}

#[test]
fn connecting_to_a_wildcard_is_refused() {
    LocalRuntime::new().unwrap().block_on(async {
        for endpoint in [
            "tcp://*:5555",
            "tcp://*:*",
            "tcp://127.0.0.1:*",
            "tcp://127.0.0.1:0",
        ] {
            let mut dealer = DealerSocket::with_options(opts());
            let refused = dealer.connect(endpoint).await;
            assert!(
                matches!(refused, Err(MonocoqueError::InvalidEndpoint(_))),
                "connect({endpoint}) gave {refused:?}"
            );
        }
    });
}

/// `tcp://[::1]:*` used to fail to parse: the `*` sent it down the host-name
/// path, brackets and all.
#[test]
fn an_ipv6_address_takes_a_port_wildcard() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut server = DealerSocket::with_options(opts());
        let bound = match server.bind("tcp://[::1]:*").await {
            Ok(bound) => bound,
            // A machine without IPv6 cannot bind it. The parsing is covered
            // by the unit tests either way.
            Err(MonocoqueError::Io(e)) => {
                eprintln!("skipped, no IPv6 loopback here: {e}");
                return;
            }
            Err(other) => panic!("bind failed: {other:?}"),
        };
        assert!(bound.starts_with("tcp://[::1]:"), "bound to {bound}");
        assert!(!bound.ends_with(":0"), "no port was picked: {bound}");

        let mut client = DealerSocket::with_options(opts());
        client.connect(&bound).await.unwrap();
        client.send("over v6").await.unwrap();
        assert_eq!(server.recv().await.unwrap(), [b"over v6"]);
    });
}
