//! A tour of the socket API: create, bind or connect, send, receive.

use monocoque::rt::LocalRuntime;
use monocoque::zmq::prelude::{
    Bytes, DealerSocket, MonocoqueError, Msg, ReqSocket, RouterSocket, SocketOptions,
};
use tracing::info;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("=== Monocoque API Examples ===\n");

    // Example 1: every socket starts unconnected, then binds or connects.
    // `bind` returns the endpoint actually bound, so port 0 picks a free one.
    info!("1. ROUTER and DEALER (using the prelude):");
    let mut router = RouterSocket::new();
    let endpoint = router.bind("tcp://127.0.0.1:0").await?;
    info!("   ROUTER bound to {endpoint}");

    let mut dealer = DealerSocket::with_options(
        SocketOptions::default().with_routing_id(Bytes::from_static(b"dealer-1")),
    );
    dealer.connect(&endpoint).await?;
    info!("   DEALER connecting as 'dealer-1'\n");

    // Example 2: messages are `Msg`, built from strings, bytes or frames.
    info!("2. Sending a multipart message:");
    let msg = Msg::from([
        Bytes::from_static(b"part one"),
        Bytes::from_static(b"part two"),
    ]);
    dealer.send(msg).await?;

    // A ROUTER receives [identity, ...] and routes replies by the first frame.
    // The request is only read, so it stays borrowed and just the identity
    // frame is kept for the reply.
    let identity = {
        let request = router.recv().await?;
        let identity = request.frame_bytes(0).unwrap_or_default();
        info!(
            "   ROUTER got {} frames from {:?}",
            request.len(),
            String::from_utf8_lossy(&identity)
        );
        identity
    };
    router
        .send(Msg::from([identity, Bytes::from_static(b"reply")]))
        .await?;

    // Example 3: `recv` lends the message; dropping the `MsgRef` releases it.
    info!("\n3. Borrowed receive:");
    {
        let got = dealer.recv().await?;
        info!(
            "   DEALER got {:?}",
            String::from_utf8_lossy(got.first().unwrap_or_default())
        );
        assert_eq!(got, [b"reply"]);
    }

    // Example 4: errors are `MonocoqueError`.
    info!("\n4. Errors:");
    let mut req = ReqSocket::new();
    match req.recv().await {
        Err(MonocoqueError::InvalidState(why)) => info!("   REQ recv before send: {why}"),
        other => info!("   unexpected: {:?}", other.map(|m| m.len())),
    }

    demonstrate_api_usage();

    Ok(())
}

fn demonstrate_api_usage() {
    info!("\n=== API Organization ===");
    info!("DealerSocket, RouterSocket: async request/reply");
    info!("ReqSocket, RepSocket: strict request/reply");
    info!("PubSocket, SubSocket, XPubSocket, XSubSocket: publish/subscribe");
    info!("PushSocket, PullSocket: pipelines");
    info!("PairSocket: one exclusive peer");
    info!("Msg, MsgRef, SocketOptions, MonocoqueError: in the prelude");
}
