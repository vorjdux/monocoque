//! Two ways to import the socket API.
//!
//! Everything ZeroMQ lives under `monocoque::zmq`, behind the `zmq` Cargo
//! feature. Name the types you use from that module, or glob-import
//! `monocoque::zmq::prelude` for the socket types, `Msg`, `SocketOptions`,
//! `Result` and `Bytes` at once. Less common parts stay in their own modules:
//! `zmq::proxy`, `zmq::poll`, `zmq::security`, and `zmq::draft` with the
//! `draft` feature.
//!
//! Run with: `cargo run --example protocol_namespaces --features zmq`

use monocoque::rt::LocalRuntime;

/// Explicit imports: each name says where it comes from.
async fn explicit_style() -> monocoque::zmq::Result<()> {
    use monocoque::zmq::{RepSocket, ReqSocket};

    let mut rep = RepSocket::new();
    let endpoint = rep.bind("tcp://127.0.0.1:0").await?;
    let mut req = ReqSocket::new();
    req.connect(&endpoint).await?;

    req.send("explicit imports").await?;
    {
        let request = rep.recv().await?;
        println!(
            "use monocoque::zmq::{{RepSocket, ReqSocket}}: REP got {:?}",
            String::from_utf8_lossy(request.first().unwrap_or_default())
        );
    }
    rep.send("ok").await?;
    req.recv().await?;

    req.close().await?;
    rep.close().await
}

/// The prelude: one import for a program that uses several socket types.
async fn prelude_style() -> monocoque::zmq::Result<()> {
    use monocoque::zmq::prelude::*;

    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    let mut dealer = DealerSocket::with_options(
        SocketOptions::default().with_routing_id(Bytes::from_static(b"dealer-1")),
    );
    dealer.connect(&endpoint).await?;

    dealer
        .send(Msg::from([
            Bytes::from_static(b"prelude"),
            Bytes::from_static(b"imports"),
        ]))
        .await?;
    {
        let msg: MsgRef<'_> = router.recv().await?;
        println!(
            "use monocoque::zmq::prelude::*: ROUTER got {} frames from {:?}",
            msg.len(),
            String::from_utf8_lossy(msg.first().unwrap_or_default())
        );
    }

    dealer.close().await?;
    router.close().await
}

fn main() -> monocoque::zmq::Result<()> {
    LocalRuntime::new()?.block_on(async {
        explicit_style().await?;
        prelude_style().await
    })
}
