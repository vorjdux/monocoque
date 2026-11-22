//! CHANNEL: one exclusive two-way peer, single-part messages (draft).
//!
//! CHANNEL is the draft counterpart of PAIR. Each socket keeps exactly one
//! peer and either side may send at any time. The difference from PAIR is
//! that the messages are single-part: sending more than one frame is an
//! error.
//!
//! Use it for a private two-way link between two parts of a program, such as
//! a controller and the task it steers, when both ends are draft-aware.
//!
//! The draft socket types follow libzmq's draft API, which may still change,
//! so they sit behind the `draft` feature.
//!
//! Run with: `cargo run --example draft_channel --features zmq,draft`

use monocoque::Bytes;
use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::draft::ChannelSocket;
use monocoque::zmq::{MonocoqueError, Msg, Result};

const ENDPOINT: &str = "inproc://draft-channel";

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut controller = ChannelSocket::new();
    controller.bind(ENDPOINT).await?;

    let worker = rt::spawn(worker());

    // Either side may speak first and neither owes the other a reply.
    command(&mut controller, "start").await?;
    command(&mut controller, "status").await?;

    // Single-part only: a two-frame message is refused before it is sent.
    let two_frames = Msg::from(["two", "frames"].map(Bytes::from));
    match controller.send(two_frames).await {
        Err(MonocoqueError::InvalidState(why)) => {
            println!("[controller] two frames refused: {why}");
        }
        other => println!("[controller] unexpected: {other:?}"),
    }

    command(&mut controller, "stop").await?;
    rt::join(worker).await?;
    controller.close().await
}

/// Send one command and print the worker's answer.
async fn command(controller: &mut ChannelSocket, command: &'static str) -> Result<()> {
    controller.send(command).await?;
    // The answer is only printed, so it stays borrowed from the socket.
    let answer = controller.recv().await?;
    println!(
        "[controller] {command} -> {}",
        String::from_utf8_lossy(answer.first().unwrap_or_default())
    );
    Ok(())
}

/// The other end of the channel: answer each command until told to stop.
async fn worker() -> Result<()> {
    let mut channel = ChannelSocket::new();
    channel.connect(ENDPOINT).await?;
    loop {
        // The command is only read, so it stays borrowed from the socket
        // until the answer is chosen.
        let (answer, stop) = {
            let command = channel.recv().await?;
            match command.first().unwrap_or_default() {
                b"start" => ("started", false),
                b"status" => ("running", false),
                b"stop" => ("stopped", true),
                _ => ("unknown command", false),
            }
        };
        channel.send(answer).await?;
        if stop {
            break;
        }
    }
    channel.close().await
}
