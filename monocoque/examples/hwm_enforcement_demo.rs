//! Demonstration of HWM (High Water Mark) enforcement in `DealerSocket`
//!
//! This example shows how `send_hwm` prevents unbounded message buffering.
//! A send to a peer at its HWM waits for room; with a zero `send_timeout` it
//! fails at once with `MonocoqueError::Again` instead.
//!
//! Run with: cargo run --package monocoque --features zmq --example `hwm_enforcement_demo`

use monocoque::rt::{self, LocalRuntime};
use monocoque::zmq::{DealerSocket, MonocoqueError, RouterSocket, SocketOptions};
use std::time::Duration;

const HWM: usize = 5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    LocalRuntime::new()?.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!("HWM Enforcement Demo\n");

    // Composable options: a small HWM, and never wait for room
    let options = SocketOptions::default()
        .with_send_hwm(HWM)
        .with_send_timeout(Duration::ZERO);

    println!("Configuration:");
    println!("  send_hwm: {} messages", options.send_hwm);
    println!("  send_timeout: zero (fail with Again instead of waiting)\n");

    // The inproc transport has no kernel buffers in the way, so the HWM is
    // the only thing that can hold messages back.
    let mut router = RouterSocket::with_options(SocketOptions::default().with_recv_hwm(HWM));
    router.bind("inproc://hwm-demo").await?;

    let mut dealer = DealerSocket::with_options(options);
    dealer.connect("inproc://hwm-demo").await?;

    println!("Sending messages (HWM = {HWM}) while the ROUTER is not reading...\n");

    let mut sent = 0;
    for i in 0..50 {
        match dealer.send(format!("Message {i}")).await {
            Ok(()) => {
                sent += 1;
                print!(".");
            }
            Err(MonocoqueError::Again) => {
                println!("\n\nHWM ENFORCED: blocked after {sent} messages");
                println!("Socket refused message #{i} instead of buffering it");
                println!("\nApplication must either:");
                println!("   - Wait (a nonzero or no send_timeout)");
                println!("   - Drop messages");
                println!("   - Let the peer drain");
                break;
            }
            Err(e) => {
                println!("\nUnexpected error: {e}");
                break;
            }
        }
    }

    // The ROUTER drains the queue, which makes room again
    let mut drained = 0;
    while drained < sent {
        router.recv().await?;
        drained += 1;
    }
    println!("\n[SERVER] Drained {drained} messages");
    rt::sleep(Duration::from_millis(10)).await;

    let after = dealer.send("after drain").await;
    println!(
        "Send after drain: {}",
        if after.is_ok() { "ok" } else { "refused" }
    );

    println!("\nResults:");
    println!("  Messages queued before refusal: {sent}");
    println!("  HWM limit: {HWM}");
    println!(
        "  Status: {}",
        if sent < 50 && after.is_ok() {
            "PASS"
        } else {
            "FAIL"
        }
    );

    println!("\nDemo complete!");
    println!("   HWM enforcement prevents unbounded memory growth");
    println!("   Applications have explicit control over buffering");

    Ok(())
}
