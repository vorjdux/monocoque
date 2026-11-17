//! RADIO/DISH: publish to named groups (draft).
//!
//! RADIO and DISH are the draft counterpart of PUB and SUB. A RADIO sends
//! each message to a group, and a DISH receives the groups it has joined.
//! Two differences from PUB/SUB topics:
//!
//! - a group is matched exactly, not by prefix: joining "weather" does not
//!   bring in "weather.eu";
//! - the group travels beside the message instead of being its first bytes,
//!   and the message itself is single-part. A DISH receives `[group, body]`.
//!
//! As with PUB, a RADIO never waits: a message for a group nobody has joined
//! is dropped, and `send` returns how many peers it went to.
//!
//! The draft socket types follow libzmq's draft API, which may still change,
//! so they sit behind the `draft` feature. Both ends have to be draft-aware.
//!
//! Run with: `cargo run --example draft_radio_dish --features zmq,draft`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::Result;
use monocoque::zmq::draft::{DishSocket, RadioSocket};
use std::time::Duration;

const REPORTS: usize = 3;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut radio = RadioSocket::new();
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = radio.bind("tcp://127.0.0.1:0").await?;
    println!("[RADIO] listening on {endpoint}");

    let mut dish = DishSocket::new();
    dish.connect(&endpoint).await?;
    dish.join("weather").await?;
    println!("[DISH] joined \"weather\"");

    // The JOIN has to reach the RADIO before anything is delivered. `send`
    // says how many peers a message went to, so the first report is offered
    // until the DISH is counted.
    while radio.send("weather", "report 1").await? == 0 {
        rt::sleep(Duration::from_millis(5)).await;
    }
    for i in 2..=REPORTS {
        radio.send("weather", format!("report {i}")).await?;
    }

    // Nobody joined these groups. The second shows that matching is exact.
    for group in ["sports", "weather.eu"] {
        let reached = radio.send(group, "not for this dish").await?;
        println!("[RADIO] \"{group}\" went to {reached} peer(s)");
    }

    for _ in 0..REPORTS {
        // The message is only printed, so it stays borrowed from the socket.
        let msg = dish.recv().await?;
        println!(
            "[DISH] group \"{}\": {}",
            String::from_utf8_lossy(msg.get(0).unwrap_or_default()),
            String::from_utf8_lossy(msg.get(1).unwrap_or_default())
        );
    }

    dish.close().await?;
    radio.close().await
}
