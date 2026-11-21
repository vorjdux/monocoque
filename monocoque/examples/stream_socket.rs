//! STREAM: talking to a plain TCP peer.
//!
//! A `StreamSocket` speaks raw TCP with no ZeroMQ protocol on the wire, so
//! its peers are ordinary TCP programs: here a `std::net::TcpStream`. Use it
//! to bridge something that is not ZeroMQ (a line protocol, an HTTP client)
//! into a program built on sockets.
//!
//! Every message has two frames, as with libzmq's `ZMQ_STREAM`:
//!
//! - a receive yields `[identity, data]`, where the identity names the
//!   connection the bytes came from;
//! - `[identity, ""]` is a notification: the peer connected, or it went away;
//! - a send of `[identity, data]` writes the data to that connection, and
//!   `[identity, ""]` closes it.
//!
//! TCP is a byte stream: the data frame holds whatever bytes arrived
//! together, not a message the peer framed.
//!
//! Run with: `cargo run --example stream_socket --features zmq`

use monocoque::Bytes;
use monocoque::rt::LocalRuntime;
use monocoque::zmq::{Msg, StreamSocket};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::thread;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const GREETING: &[u8] = b"hello";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut server = StreamSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = server.bind("tcp://127.0.0.1:0").await?;
    println!("[STREAM] listening on {endpoint}");

    // The peer is blocking code, so it gets a thread of its own.
    let address = endpoint.trim_start_matches("tcp://").to_owned();
    let client = thread::spawn(move || tcp_client(&address));

    // First comes the connect notification. The identity is all that is
    // kept from it: it addresses this connection from now on.
    let identity = {
        let msg = server.recv().await?;
        let identity = msg.frame_bytes(0).unwrap_or_default();
        println!(
            "[STREAM] connect notification: identity {:02x?}, {} data bytes",
            &identity[..],
            msg.get(1).map_or(0, <[u8]>::len)
        );
        identity
    };

    // Then the bytes the peer wrote, under the same identity.
    let reply = {
        let msg = server.recv().await?;
        let data = msg.get(1).unwrap_or_default();
        println!(
            "[STREAM] {} bytes from {:02x?}: {:?}",
            data.len(),
            msg.first().unwrap_or_default(),
            String::from_utf8_lossy(data)
        );
        data.to_ascii_uppercase()
    };

    // The identity frame picks the connection; the rest is written to it.
    server
        .send(Msg::from([identity.clone(), Bytes::from(reply)]))
        .await?;

    // The peer reads the reply and hangs up, which arrives as a second
    // notification for the same identity.
    {
        let msg = server.recv().await?;
        let gone = msg.first() == Some(&identity[..]) && msg.get(1).is_some_and(<[u8]>::is_empty);
        println!("[STREAM] disconnect notification for the same identity: {gone}");
    }

    client.join().map_err(|_| "the TCP client panicked")??;
    server.close().await?;
    Ok(())
}

/// An ordinary TCP client: no ZeroMQ involved.
fn tcp_client(address: &str) -> std::io::Result<()> {
    let mut stream = TcpStream::connect(address)?;
    stream.write_all(GREETING)?;

    let mut reply = [0u8; GREETING.len()];
    stream.read_exact(&mut reply)?;
    println!("[TCP client] got {:?}", String::from_utf8_lossy(&reply));
    // Dropping the stream closes the connection.
    Ok(())
}
