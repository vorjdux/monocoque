//! `zmq_poll`: one task serving two sockets of different kinds.
//!
//! A worker takes jobs from a PULL socket and listens for a stop command on a
//! SUB socket. `poll` waits on both at once and reports which is ready; it
//! never takes a message itself, so each ready socket is then read normally.
//! The message's connection metadata (`zmq_msg_gets`) says who sent it.
//!
//! Run with: `cargo run --example poll_demo --features zmq`

use monocoque::rt::LocalRuntime;
use monocoque::zmq::poll::{PollEvents, PollItem, poll};
use monocoque::zmq::{PubSocket, PullSocket, PushSocket, SocketOptions, SubSocket};
use std::time::Duration;

fn main() -> monocoque::zmq::Result<()> {
    LocalRuntime::new()?.block_on(async {
        let mut jobs = PullSocket::new();
        let jobs_ep = jobs.bind("tcp://127.0.0.1:0").await?;
        let mut control = SubSocket::new();
        let mut control_pub = PubSocket::new();
        let control_ep = control_pub.bind("tcp://127.0.0.1:0").await?;
        control.connect(&control_ep).await?;
        control.subscribe("").await?;

        let mut producer =
            PushSocket::with_options(SocketOptions::default().with_metadata("X-Producer", "demo"));
        producer.connect(&jobs_ep).await?;
        for i in 0..3 {
            producer.send(format!("job {i}")).await?;
        }

        let mut done = 0;
        loop {
            let mut items = [
                PollItem::new(&jobs, PollEvents::POLLIN),
                PollItem::new(&control, PollEvents::POLLIN),
            ];
            let ready = poll(&mut items, Some(Duration::from_secs(5))).await?;
            if ready == 0 {
                println!("nothing for 5s, giving up");
                break;
            }
            let (job_ready, stop_ready) = (items[0].is_readable(), items[1].is_readable());
            if job_ready {
                let job = jobs.recv().await?;
                println!(
                    "{} (from a {} peer, X-Producer = {:?})",
                    String::from_utf8_lossy(job.get(0).unwrap_or_default()),
                    job.gets("Socket-Type").unwrap_or("?"),
                    job.gets("X-Producer"),
                );
                done += 1;
                if done == 3 {
                    // Wait for the subscription to land, then tell ourselves
                    // to stop.
                    while control_pub.peer_count() == 0 {
                        monocoque::rt::sleep(Duration::from_millis(1)).await;
                    }
                    monocoque::rt::sleep(Duration::from_millis(50)).await;
                    control_pub.send("stop").await?;
                }
            }
            if stop_ready {
                let cmd = control.recv().await?;
                println!("control: {}", String::from_utf8_lossy(cmd.get(0).unwrap_or_default()));
                break;
            }
        }
        Ok(())
    })
}
