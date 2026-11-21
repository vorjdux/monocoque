//! Conflate: a slow reader that only wants the latest value.
//!
//! With `with_conflate(true)` a receiving socket keeps one message per peer,
//! the newest. Each arrival replaces the one still waiting, so a reader that
//! falls behind skips ahead instead of working through a backlog, and the
//! receive HWM no longer applies.
//!
//! Use it for state that goes stale: sensor readings, prices, positions. Do
//! not use it where every message matters, since the ones in between are
//! gone.
//!
//! Here two sensors each push 100 readings while the display is busy. When
//! the display reads, it finds two messages: the last reading of each sensor.
//!
//! Run with: `cargo run --example conflate --features zmq`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{MonocoqueError, PullSocket, PushSocket, Result, SocketOptions};
use std::time::Duration;

const READINGS: usize = 100;

fn main() -> Result<()> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let mut display = PullSocket::with_options(
        SocketOptions::default()
            .with_conflate(true)
            // Lets the last receive below report that nothing else is queued.
            .with_recv_timeout(Duration::from_millis(200)),
    );
    // Port 0: the OS picks a free port and `bind` reports it.
    let endpoint = display.bind("tcp://127.0.0.1:0").await?;

    let mut sensors = Vec::new();
    for name in ["temperature", "pressure"] {
        let mut sensor = PushSocket::new();
        sensor.connect(&endpoint).await?;
        for reading in 1..=READINGS {
            sensor.send(format!("{name} reading {reading}")).await?;
        }
        println!("[{name}] sent {READINGS} readings");
        sensors.push(sensor);
    }

    // The display is busy while the readings arrive. The socket keeps taking
    // them off the network in the background, replacing as it goes.
    rt::sleep(Duration::from_millis(200)).await;

    let mut kept = 0;
    loop {
        match display.recv().await {
            Ok(msg) => {
                kept += 1;
                println!(
                    "[display] {}",
                    String::from_utf8_lossy(msg.first().unwrap_or_default())
                );
            }
            Err(MonocoqueError::TimedOut) => break,
            Err(e) => return Err(e),
        }
    }
    println!(
        "[display] {kept} messages were waiting, of {} sent",
        READINGS * sensors.len()
    );

    for sensor in sensors {
        sensor.close().await?;
    }
    display.close().await
}
