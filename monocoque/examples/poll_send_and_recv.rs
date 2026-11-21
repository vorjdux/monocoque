//! `zmq_poll` for sending and receiving in one loop.
//!
//! A task that sends requests on one socket and reads the answers on another
//! can deadlock itself. If it sends everything first, the answers pile up
//! unread until every queue between the two sockets is full; then the peer
//! stops taking requests and the next `send` waits for room that only reading
//! would make.
//!
//! The way out without a second task is to ask both questions at once: `poll`
//! with POLLOUT on the sending socket and POLLIN on the receiving one, then
//! do whichever is possible. POLLOUT means a send would be accepted without
//! waiting, POLLIN that a receive would return at once, so the loop never
//! blocks on one side while the other needs attention.
//!
//! Here a PUSH sends to an echo peer that sends everything back to a PULL.
//! Every queue holds four messages, far fewer than are sent, so sending them
//! all before reading any could not finish.
//!
//! Use this shape when one task owns both directions. When the two can be
//! separate tasks, as in pipeline_worker_pool.rs, that is simpler.
//!
//! Run with: `cargo run --example poll_send_and_recv --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::poll::{PollEvents, PollItem, poll};
use monocoque::zmq::{PullSocket, PushSocket, Result, SocketOptions};
use std::time::Duration;

const MESSAGES: usize = 10_000;
const HWM: usize = 4;
const OUT: &str = "inproc://poll-send-and-recv-out";
const BACK: &str = "inproc://poll-send-and-recv-back";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    // The inproc transport has no kernel buffers, so the HWMs are the only
    // room there is.
    let bounded = SocketOptions::default()
        .with_send_hwm(HWM)
        .with_recv_hwm(HWM);

    let mut out = PushSocket::with_options(bounded.clone());
    out.bind(OUT).await?;
    let mut back = PullSocket::with_options(bounded.clone());
    back.bind(BACK).await?;

    let echo = rt::spawn(echo(bounded));

    let (mut sent, mut received) = (0usize, 0usize);
    let mut longest_lead = 0usize;
    while received < MESSAGES {
        let (can_send, can_recv) = {
            // Once everything is sent there is nothing to ask the sending
            // socket; an item with no events is skipped.
            let sending = if sent < MESSAGES {
                PollEvents::POLLOUT
            } else {
                PollEvents::NONE
            };
            let mut items = [
                PollItem::new(&out, sending),
                PollItem::new(&back, PollEvents::POLLIN),
            ];
            if poll(&mut items, Some(Duration::from_secs(5))).await? == 0 {
                println!("nothing to do for 5 s after {sent} sent, {received} received");
                break;
            }
            (items[0].is_writable(), items[1].is_readable())
        };
        if can_send {
            out.send(format!("message {sent}")).await?;
            sent += 1;
        }
        if can_recv {
            // The echo is only counted, so it stays borrowed from the socket.
            back.recv().await?;
            received += 1;
        }
        longest_lead = longest_lead.max(sent - received);
    }

    println!("sent {sent} and received {received} through queues of {HWM}");
    println!("at most {longest_lead} messages were in flight at once");

    // A receive does not fail when the peer goes away, so the echo peer is
    // told to stop: an empty message is the end marker.
    out.send("").await?;
    rt::join(echo).await?;
    out.close().await?;
    back.close().await
}

/// Send back everything received, up to the empty message that ends the run.
async fn echo(bounded: SocketOptions) -> Result<()> {
    let mut input = PullSocket::with_options(bounded.clone());
    input.connect(OUT).await?;
    let mut output = PushSocket::with_options(bounded);
    output.connect(BACK).await?;

    loop {
        // The message goes back out as it came, so it is taken by value.
        let msg = input.recv_owned().await?;
        if msg.first().is_none_or(<[u8]>::is_empty) {
            break;
        }
        output.send(msg).await?;
    }
    output.close().await
}
