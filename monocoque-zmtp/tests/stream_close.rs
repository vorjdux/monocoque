//! STREAM delivers everything a raw peer sent before it closed, then the
//! disconnect notification.

use monocoque_core::options::SocketOptions;
use monocoque_core::rt::LocalRuntime;
use monocoque_zmtp::sock::stream::StreamSocket;
use std::io::Write;
use std::time::Duration;

#[test]
fn data_sent_just_before_close_arrives_before_the_disconnect() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut s = StreamSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let ep = s.bind("tcp://127.0.0.1:0").await.unwrap();
        let addr = ep.trim_start_matches("tcp://").to_string();
        for round in 0..50 {
            let addr = addr.clone();
            let client = std::thread::spawn(move || {
                let mut c = std::net::TcpStream::connect(addr).unwrap();
                c.write_all(&[7u8; 1000]).unwrap();
            });
            let mut got = 0;
            let mut events = 0;
            while events < 2 {
                let m = s.recv_owned().await.unwrap();
                let body = m.frames().nth(1).map_or(0, <[u8]>::len);
                if body == 0 {
                    events += 1;
                } else {
                    assert_eq!(events, 1, "round {round}: data after the disconnect");
                    got += body;
                }
            }
            client.join().unwrap();
            assert_eq!(got, 1000, "round {round}");
        }
    });
}

#[test]
fn close_after_send_writes_everything_first() {
    use std::io::Read;
    LocalRuntime::new().unwrap().block_on(async {
        let mut s = StreamSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let ep = s.bind("tcp://127.0.0.1:0").await.unwrap();
        let addr = ep.trim_start_matches("tcp://").to_string();
        for round in 0..20 {
            let addr = addr.clone();
            let client = std::thread::spawn(move || {
                let mut c = std::net::TcpStream::connect(addr).unwrap();
                let mut all = Vec::new();
                c.read_to_end(&mut all).unwrap();
                all.len()
            });
            let id = s.recv_owned().await.unwrap().first().unwrap().to_vec();
            let id = bytes::Bytes::from(id);
            let body = bytes::Bytes::from(vec![3u8; 256 * 1024]);
            for _ in 0..4 {
                s.send(monocoque_core::msg::Msg::from([id.clone(), body.clone()]))
                    .await
                    .unwrap();
            }
            s.send(monocoque_core::msg::Msg::from([
                id.clone(),
                bytes::Bytes::new(),
            ]))
            .await
            .unwrap();
            // Keep the runtime turning so the socket can write.
            while !client.is_finished() {
                monocoque_core::rt::sleep(Duration::from_millis(1)).await;
            }
            let n = client.join().unwrap();
            assert_eq!(n, 4 * 256 * 1024, "round {round}");
            // The disconnect notification.
            s.recv_owned().await.unwrap();
        }
    });
}

#[test]
fn messages_and_notifications_carry_the_peer_address() {
    LocalRuntime::new().unwrap().block_on(async {
        let mut s = StreamSocket::with_options(
            SocketOptions::default().with_recv_timeout(Duration::from_secs(5)),
        );
        let ep = s.bind("tcp://127.0.0.1:0").await.unwrap();
        let addr = ep.trim_start_matches("tcp://").to_string();
        let client = std::thread::spawn(move || {
            let mut c = std::net::TcpStream::connect(addr).unwrap();
            c.write_all(b"hello").unwrap();
        });
        // Connect, data, disconnect: every one says who it is about.
        for _ in 0..3 {
            let m = s.recv().await.unwrap();
            assert_eq!(m.gets("Peer-Address"), Some("127.0.0.1"));
        }
        client.join().unwrap();
    });
}
