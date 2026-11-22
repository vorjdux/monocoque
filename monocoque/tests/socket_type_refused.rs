//! A peer whose READY names a socket type this socket cannot talk to is told
//! so with an ERROR command, and then the connection is closed (RFC 23, "The
//! Socket-Type Property").

use monocoque::rt::LocalRuntime;
use monocoque::zmq::PubSocket;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// A PUB socket kept alive for `life`. Returns its endpoint.
fn pub_socket_for(life: Duration) -> String {
    let (ep_tx, ep_rx) = mpsc::channel();
    thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut publisher = PubSocket::new();
            ep_tx
                .send(publisher.bind("tcp://127.0.0.1:0").await.unwrap())
                .unwrap();
            monocoque::rt::sleep(life).await;
        });
    });
    ep_rx.recv().unwrap()
}

/// Do a ZMTP 3 NULL handshake announcing `socket_type`, and return every
/// byte the socket sends after its greeting until it closes the connection,
/// or until two seconds pass with it still open (`None`).
fn bytes_after_greeting(endpoint: &str, socket_type: &[u8]) -> Option<Vec<u8>> {
    let mut s = TcpStream::connect(endpoint.trim_start_matches("tcp://")).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    s.write_all(&greeting).unwrap();
    let mut peer = [0u8; 64];
    s.read_exact(&mut peer).unwrap();

    let mut ready = vec![5u8];
    ready.extend_from_slice(b"READY");
    ready.push(11);
    ready.extend_from_slice(b"Socket-Type");
    ready.extend_from_slice(&u32::try_from(socket_type.len()).unwrap().to_be_bytes());
    ready.extend_from_slice(socket_type);
    s.write_all(&[0x04, u8::try_from(ready.len()).unwrap()])
        .unwrap();
    s.write_all(&ready).unwrap();

    let mut sent = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => return Some(sent),
            Ok(n) => sent.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
}

#[test]
fn a_refused_socket_type_gets_an_error_command_and_a_close() {
    let endpoint = pub_socket_for(Duration::from_secs(5));
    let sent = bytes_after_greeting(&endpoint, b"PUB").expect("the connection was left open");

    let reason = b"Incompatible socket type";
    let mut error = vec![0x04, u8::try_from(7 + reason.len()).unwrap(), 5];
    error.extend_from_slice(b"ERROR");
    error.push(u8::try_from(reason.len()).unwrap());
    error.extend_from_slice(reason);
    assert!(
        sent.ends_with(&error),
        "the last thing sent was not the ERROR command: {sent:?}"
    );
}

#[test]
fn an_accepted_socket_type_gets_no_error_command() {
    let endpoint = pub_socket_for(Duration::from_secs(5));
    assert!(
        bytes_after_greeting(&endpoint, b"SUB").is_none(),
        "a SUB peer was disconnected by a PUB socket"
    );
}
