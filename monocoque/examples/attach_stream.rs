//! `attach`: hand a socket a stream that is already connected.
//!
//! `bind` and `connect` make the connections themselves. `attach(stream)`
//! takes one made elsewhere, runs the ZMTP handshake on it and adds it as a
//! peer. It accepts the `TcpStream` and `UnixStream` types from
//! `monocoque::rt`.
//!
//! Use it when something else owns how connections are made: a listener the
//! program already runs, a connection handed over by a supervisor, a dial
//! that needs options the socket does not expose. An attached stream is not
//! redialed when it drops; reconnecting is up to whoever made it.
//!
//! `attach` returns once the handshake is done, and a handshake needs both
//! ends talking, so here the two ends attach in different tasks.
//!
//! Run with: `cargo run --example attach_stream --features zmq`

use monocoque::rt::{self, LocalRuntime, TcpListener, TcpStream};
use monocoque::zmq::{RepSocket, ReqSocket};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // A plain TCP listener and a plain TCP connection, made without any
    // socket knowing about them.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    println!("listening on {address}");

    let server = rt::spawn(async move {
        let (stream, from) = listener.accept().await?;
        println!("[REP] accepted a TCP connection from {from}");

        let mut rep = RepSocket::new();
        // `attach` returns the id of the new peer.
        rep.attach(stream).await?;
        println!("[REP] handshake done, {} peer", rep.peer_count());

        // The request is only read, so it is turned into the reply before
        // the next call on the socket.
        let reply = {
            let request = rep.recv().await?;
            let text = String::from_utf8_lossy(request.first().unwrap_or_default());
            println!("[REP] got \"{text}\"");
            format!("reply to: {text}")
        };
        rep.send(reply).await?;
        rep.close().await
    });

    let stream = TcpStream::connect(address).await?;
    println!("[REQ] TCP connection to {address} is up");

    let mut req = ReqSocket::new();
    req.attach(stream).await?;
    println!("[REQ] handshake done, {} peer", req.peer_count());

    req.send("hello over an attached stream").await?;
    {
        let reply = req.recv().await?;
        println!(
            "[REQ] got \"{}\"",
            String::from_utf8_lossy(reply.first().unwrap_or_default())
        );
    }
    req.close().await?;

    rt::join(server).await?;
    Ok(())
}
