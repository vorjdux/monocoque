//! Protocol commands and messages never interleave on the wire.
//!
//! A peer that floods PINGs and reads slowly keeps a job of PONGs, which
//! count as no message, out with the writer while the application sends. A
//! direct write from `send` must not go around that job: it would land inside
//! the PONGs or ahead of them and corrupt the stream.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, PushSocket};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const PAYLOAD: [u8; 32] = [0xAB; 32];
const PING: &[u8] = b"\x04\x0b\x04PING\x00\x0a1234";

/// Answer a ZMTP 3 NULL handshake as a PULL socket.
fn handshake(s: &mut TcpStream) {
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
    ready.extend_from_slice(&4u32.to_be_bytes());
    ready.extend_from_slice(b"PULL");
    s.write_all(&[0x04, u8::try_from(ready.len()).unwrap()])
        .unwrap();
    s.write_all(&ready).unwrap();
}

/// Parse frames from `buf`, returning how many bytes were consumed. Panics on
/// anything but a whole data frame of `PAYLOAD` or a whole command frame
/// (the peer's READY, or a PONG).
fn parse(buf: &[u8], data: &mut usize, pongs: &mut usize) -> usize {
    let mut at = 0;
    loop {
        let rest = &buf[at..];
        if rest.len() < 2 {
            return at;
        }
        let flags = rest[0];
        assert_eq!(
            flags & !0x07,
            0,
            "reserved flag bits at byte {at}: stream corrupted"
        );
        let (header, len) = if flags & 0x02 != 0 {
            if rest.len() < 9 {
                return at;
            }
            let raw = u64::from_be_bytes(rest[1..9].try_into().unwrap());
            (9, usize::try_from(raw).unwrap())
        } else {
            (2, usize::from(rest[1]))
        };
        assert!(
            len < 1 << 16,
            "frame of {len} bytes at byte {at}: stream corrupted"
        );
        if rest.len() < header + len {
            return at;
        }
        let body = &rest[header..header + len];
        if flags & 0x04 != 0 {
            let name_len = usize::from(body[0]);
            let name = &body[1..=name_len];
            assert!(
                name == b"READY" || name == b"PONG",
                "command {:?} at byte {at}: stream corrupted",
                String::from_utf8_lossy(name)
            );
            *pongs += usize::from(name == b"PONG");
        } else {
            assert_eq!(body, PAYLOAD, "data frame at byte {at}: stream corrupted");
            *data += 1;
        }
        at += header + len;
    }
}

#[test]
fn a_ping_flood_does_not_interleave_pongs_with_messages() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ep = format!("tcp://{}", listener.local_addr().unwrap());
    let done = Arc::new(AtomicBool::new(false));
    let sender_gone = Arc::new(AtomicBool::new(false));

    let peer = {
        let done = Arc::clone(&done);
        let sender_gone = Arc::clone(&sender_gone);
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            handshake(&mut s);
            let mut flood = s.try_clone().unwrap();
            let pinger = {
                let done = Arc::clone(&done);
                thread::spawn(move || {
                    let burst = PING.repeat(256);
                    while !done.load(Ordering::Relaxed) && flood.write_all(&burst).is_ok() {}
                })
            };
            // Read slowly so the socket's send buffer, PONGs and all, fills
            // and its writer keeps a job out.
            s.set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            let (mut buf, mut have) = (vec![0u8; 1 << 20], 0);
            let (mut data, mut pongs) = (0, 0);
            let deadline = Instant::now() + Duration::from_secs(4);
            // After the deadline keep reading, now at full speed, until the
            // sender has seen `done` and left: closing first would leave its
            // `send` waiting for a peer that is gone.
            while !sender_gone.load(Ordering::Relaxed) {
                let slow = Instant::now() < deadline;
                if slow {
                    thread::sleep(Duration::from_micros(200));
                } else {
                    done.store(true, Ordering::Relaxed);
                }
                let room = if slow { 512 } else { buf.len() - have };
                match s.read(&mut buf[have..have + room]) {
                    Ok(0) => break,
                    Ok(n) => have += n,
                    Err(_) => continue,
                }
                let used = parse(&buf[..have], &mut data, &mut pongs);
                buf.copy_within(used..have, 0);
                have -= used;
            }
            done.store(true, Ordering::Relaxed);
            drop(s);
            pinger.join().unwrap();
            (data, pongs)
        })
    };

    let sender = {
        let done = Arc::clone(&done);
        let sender_gone = Arc::clone(&sender_gone);
        thread::spawn(move || {
            LocalRuntime::new().unwrap().block_on(async move {
                let mut push = PushSocket::new();
                push.connect(&ep).await.unwrap();
                let msg = Msg::copy_from_slice(&PAYLOAD);
                let mut sent = 0u32;
                while !done.load(Ordering::Relaxed) {
                    if push.send(msg.clone()).await.is_err() {
                        break;
                    }
                    // A send with room finishes without yielding; give the
                    // runtime a turn now and then so the socket's reader sees
                    // the PINGs, as any loop with other work would.
                    sent = sent.wrapping_add(1);
                    if sent.is_multiple_of(64) {
                        monocoque::rt::sleep(Duration::ZERO).await;
                    }
                }
            });
            sender_gone.store(true, Ordering::Relaxed);
        })
    };

    let (data, pongs) = peer.join().unwrap();
    sender.join().unwrap();
    assert!(
        data > 0 && pongs > 0,
        "the run exercised both: {data} messages, {pongs} PONGs"
    );
}

/// CURVE numbers every frame, and a frame that reaches the wire ahead of an
/// earlier one breaks the sequence: the peer drops the connection. With
/// heartbeats every millisecond on both sides, PINGs and PONGs mix with the
/// data the whole time; the connection must last.
#[test]
fn curve_with_heartbeats_keeps_its_frames_in_order() {
    use monocoque::zmq::{PullSocket, SocketOptions};
    use monocoque_zmtp::security::curve::CurveSecretKey;

    let key = |seed: u8| {
        let secret = [seed; 32];
        (
            *CurveSecretKey::from_bytes(secret).public_key().as_bytes(),
            secret,
        )
    };
    let (server_public, server_secret) = key(1);
    let (client_public, client_secret) = key(2);
    let beat = Duration::from_millis(1);
    let run_for = Duration::from_secs(10);
    let done = Arc::new(AtomicBool::new(false));
    let (ep_tx, ep_rx) = std::sync::mpsc::channel::<String>();

    let puller = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            LocalRuntime::new().unwrap().block_on(async move {
                let mut pull = PullSocket::with_options(
                    SocketOptions::default()
                        .with_curve_server(true)
                        .with_curve_keypair(server_public, server_secret)
                        .with_heartbeat_ivl(beat)
                        .with_recv_timeout(Duration::from_secs(2)),
                );
                ep_tx
                    .send(pull.bind("tcp://127.0.0.1:0").await.unwrap())
                    .unwrap();
                let start = Instant::now();
                let mut got = 0u64;
                while start.elapsed() < run_for {
                    pull.recv()
                        .await
                        .unwrap_or_else(|e| panic!("connection lost after {got} messages: {e}"));
                    got += 1;
                    // Read in bursts, so the sender's buffers fill and its
                    // writer keeps jobs out.
                    if got.is_multiple_of(4096) {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                done.store(true, Ordering::Relaxed);
                // Drain until the sender has left, so it is not left waiting.
                while pull.recv().await.is_ok() {}
                got
            })
        })
    };

    let ep = ep_rx.recv().unwrap();
    let sender = thread::spawn(move || {
        LocalRuntime::new().unwrap().block_on(async move {
            let mut push = PushSocket::with_options(
                SocketOptions::default()
                    .with_curve_keypair(client_public, client_secret)
                    .with_curve_serverkey(server_public)
                    .with_heartbeat_ivl(beat),
            );
            push.connect(&ep).await.unwrap();
            let msg = Msg::copy_from_slice(&PAYLOAD);
            let mut sent = 0u32;
            while !done.load(Ordering::Relaxed) {
                push.send(msg.clone()).await.unwrap();
                sent = sent.wrapping_add(1);
                if sent.is_multiple_of(64) {
                    monocoque::rt::sleep(Duration::ZERO).await;
                }
            }
        });
    });

    sender.join().unwrap();
    let got = puller.join().unwrap();
    assert!(got > 0);
}
