//! XPUB manual mode: the publisher decides what each subscriber gets.
//!
//! An XPUB normally applies every subscription a peer sends. With
//! `with_xpub_manual(true)` it only reports them: nothing reaches a subscriber
//! until the application calls `subscribe` on the XPUB, which applies a prefix
//! of the application's choosing to the peer whose subscription message was
//! received last. `unsubscribe` takes one away the same way.
//!
//! Use it to put a policy between what a subscriber asks for and what it is
//! sent: access control, rewriting a topic, or a last value cache that
//! answers a new subscription itself.
//!
//! Here the policy is that a subscriber only ever sees the public part of a
//! topic: asking for `news` is granted as `news.public`. A SUB still filters
//! what it receives by its own subscriptions, so what is granted has to fall
//! inside what was asked for.
//!
//! `with_xpub_welcome_msg` is shown too: a message every subscriber is sent
//! as it connects, before anything is granted.
//!
//! Run with: `cargo run --example xpub_manual --features zmq`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::{
    MonocoqueError, Result, SocketOptions, SubSocket, SubscriptionEvent, XPubSocket,
};
use std::time::Duration;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut xpub = XPubSocket::with_options(
        SocketOptions::default()
            .with_xpub_manual(true)
            .with_xpub_welcome_msg("news.public.welcome".into())
            .with_recv_timeout(Duration::from_secs(5)),
    );
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = xpub.bind("tcp://127.0.0.1:0").await?;

    // The subscriber asks for everything under "news". It subscribes before
    // connecting, so its own filter already lets the welcome message in.
    let mut sub = SubSocket::with_options(
        SocketOptions::default().with_recv_timeout(Duration::from_millis(200)),
    );
    sub.subscribe("news").await?;
    sub.connect(&endpoint).await?;

    // The request is reported, not applied.
    let asked = requested(&mut xpub, true).await?;
    println!("[XPUB] a subscriber asked for {asked:?}");
    let delivered = xpub.send("news.public.1 (before the grant)").await?;
    println!("[XPUB] published news.public.1 to {delivered} subscriber(s)");

    // Grant the public part only. This applies to the peer whose
    // subscription message was received last, the one just read.
    let granted = format!("{asked}.public");
    xpub.subscribe(&granted).await?;
    println!("[XPUB] granted {granted:?} instead");

    for topic in ["news.public.2", "news.internal.1"] {
        let delivered = xpub.send(topic).await?;
        println!("[XPUB] published {topic} to {delivered} subscriber(s)");
    }

    // The subscriber got the welcome message and the one public message
    // published after the grant.
    drain(&mut sub).await?;

    // When the subscriber drops its subscription, the application takes the
    // grant back in the same way.
    sub.unsubscribe("news").await?;
    let dropped = requested(&mut xpub, false).await?;
    xpub.unsubscribe(format!("{dropped}.public")).await?;
    println!("[XPUB] the subscriber dropped {dropped:?}, grant removed");
    let delivered = xpub.send("news.public.3").await?;
    println!("[XPUB] published news.public.3 to {delivered} subscriber(s)");

    sub.close().await?;
    xpub.close().await
}

/// Wait for the next subscription (`subscribe`) or unsubscription request and
/// return its prefix.
async fn requested(xpub: &mut XPubSocket, subscribe: bool) -> Result<String> {
    loop {
        match xpub.recv_subscription().await? {
            SubscriptionEvent::Subscribe(prefix) if subscribe => {
                return Ok(String::from_utf8_lossy(&prefix).into_owned());
            }
            SubscriptionEvent::Unsubscribe(prefix) if !subscribe => {
                return Ok(String::from_utf8_lossy(&prefix).into_owned());
            }
            // The enum can grow, and the other kind is not the one awaited.
            _ => {}
        }
    }
}

/// Print everything the subscriber has received.
async fn drain(sub: &mut SubSocket) -> Result<()> {
    loop {
        match sub.recv().await {
            Ok(msg) => println!(
                "[SUB]  received {:?}",
                String::from_utf8_lossy(msg.first().unwrap_or_default())
            ),
            Err(MonocoqueError::TimedOut) => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}
