//! A CURVE-encrypted DEALER that echoes every message it receives, used by the
//! pyzmq interop tests in `interop_tests/test_curve_interop.py`.
//!
//! ```text
//! curve_echo server <endpoint> <server_public> <server_secret> <count> [heartbeat_ms]
//! curve_echo client <endpoint> <server_public> <client_public> <client_secret> <count> [heartbeat_ms]
//! ```
//!
//! Keys are 64 hex characters. The server prints the endpoint it bound once it
//! is listening; the client sends `hello` once connected. Either side then
//! echoes `count` messages and exits.

use monocoque::zmq::{DealerSocket, SocketOptions};
use std::io::Write;
use std::time::Duration;

fn key(hex_key: &str) -> [u8; 32] {
    hex::decode(hex_key)
        .expect("keys are hex")
        .try_into()
        .expect("keys are 32 bytes")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let server = args[1] == "server";
    let (count, heartbeat) = if server {
        (&args[5], args.get(6))
    } else {
        (&args[6], args.get(7))
    };
    let count: usize = count.parse().expect("count is a number");
    let mut options = SocketOptions::default().with_recv_timeout(Duration::from_secs(10));
    if let Some(ms) = heartbeat {
        let ivl = Duration::from_millis(ms.parse().expect("heartbeat is milliseconds"));
        options = options
            .with_heartbeat_ivl(ivl)
            .with_heartbeat_timeout(ivl * 4);
    }

    monocoque::rt::LocalRuntime::new()
        .unwrap()
        .block_on(async move {
            let mut socket = if server {
                let mut s = DealerSocket::with_options(
                    options
                        .with_curve_server(true)
                        .with_curve_keypair(key(&args[3]), key(&args[4])),
                );
                println!("{}", s.bind(&args[2]).await.unwrap());
                std::io::stdout().flush().unwrap();
                s
            } else {
                let mut s = DealerSocket::with_options(
                    options
                        .with_curve_serverkey(key(&args[3]))
                        .with_curve_keypair(key(&args[4]), key(&args[5])),
                );
                s.connect(&args[2]).await.unwrap();
                s.send("hello").await.unwrap();
                s
            };
            for _ in 0..count {
                let msg = socket.recv_owned().await.unwrap();
                socket.send(msg).await.unwrap();
            }
            socket.close().await.unwrap();
        });
}
