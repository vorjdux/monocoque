//! Demonstration of PoisonGuard protecting against timeout-induced stream corruption.
//!
//! This example demonstrates how PoisonGuard automatically protects against TCP
//! stream corruption when send operations timeout and are cancelled mid-write.
//!
//! ## What PoisonGuard Prevents
//!
//! When `compio::time::timeout()` expires during a send operation:
//! 1. The Future is DROPPED immediately
//! 2. TCP stream left with partial data (e.g., 500KB of 1MB message)
//! 3. Without protection: Next send() writes new header in middle of old payload
//! 4. Result: Peer receives corrupted protocol data → protocol error
//!
//! ## How to Run This Demo
//!
//! This demo tries to connect to a server at 127.0.0.1:55555 with an extremely
//! short timeout (1 nanosecond) to force cancellation during send.
//!
//! To see it in action, you can:
//! 1. Run without a server (will show connection error)
//! 2. Run with a server to see timeout and poisoning behavior

use bytes::Bytes;
use monocoque::zmq::ReqSocket;
use monocoque::SocketOptions;
use std::io;
use std::time::Duration;

#[compio::main]
async fn main() -> io::Result<()> {
    println!("\n=== PoisonGuard Protection Demo ===\n");
    println!("This demo shows how PoisonGuard automatically prevents stream corruption");
    println!("when async send operations are cancelled by timeouts.\n");

    // Create socket options with extremely short send timeout
    // This will almost certainly timeout during any send operation
    let options = SocketOptions::default()
        .with_send_timeout(Duration::from_nanos(1))
        .with_recv_timeout(Duration::from_secs(5));

    println!("1. Connecting with 1ns send timeout (will force cancellation)...");
    
    match ReqSocket::connect_with_options("tcp://127.0.0.1:55555", options).await {
        Ok(mut socket) => {
            println!("   ✓ Connected to server\n");

            // Prepare a message
            let msg = vec![Bytes::from("Hello, World!")];

            // Try to send - this will likely timeout and poison the socket
            println!("2. Attempting first send with 1ns timeout...");
            match socket.send(msg.clone()).await {
                Ok(_) => {
                    println!("   ✓ Send succeeded (timeout was long enough - unlikely!)");
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    println!("   ✗ Send timed out (expected)");
                    println!("   → Socket is now POISONED by PoisonGuard");
                }
                Err(e) => {
                    println!("   ✗ Send failed: {}", e);
                }
            }

            // Try to send again - should get BrokenPipe error from poison check
            println!("\n3. Attempting second send (poison check should trigger)...");
            match socket.send(msg).await {
                Ok(_) => {
                    println!("   ✓ Send succeeded (socket was not poisoned)");
                    println!("   This means the first send completed before timeout!");
                }
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    println!("   ✗ Got BrokenPipe error (expected!)");
                    println!("   → Error message: {}", e);
                    println!("\n✅ SUCCESS! PoisonGuard prevented reuse of corrupted socket!");
                    println!("\nWhat happened:");
                    println!("• First send timed out → Future was DROPPED mid-write");
                    println!("• PoisonGuard marked socket as poisoned");
                    println!("• Second send checked poison flag → returned BrokenPipe");
                    println!("• Application now knows it must reconnect");
                    println!("\nWithout PoisonGuard:");
                    println!("• Second send would write new data to corrupted stream");
                    println!("• Peer would receive invalid protocol data");
                    println!("• Silent corruption that's hard to debug");
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    println!("   ✗ Got TimedOut error again");
                    println!("   This means both sends timed out before completion");
                }
                Err(e) => {
                    println!("   ✗ Unexpected error: {} (kind: {:?})", e, e.kind());
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            println!("   ✗ Connection refused (no server running)\n");
            println!("To see the full demo in action:");
            println!("1. Start a ZeroMQ REP server on 127.0.0.1:55555");
            println!("2. Run this demo again");
            println!("\nEven without a server, you can see how PoisonGuard works:");
            println!("• Timeouts mark sockets as poisoned");
            println!("• Poisoned sockets return BrokenPipe on next operation");
            println!("• This prevents silent TCP stream corruption");
        }
        Err(e) => {
            println!("   ✗ Connection failed: {}\n", e);
        }
    }

    println!("\n=== Key Takeaways ===\n");
    println!("Protection: PoisonGuard is RAII-based (automatic, no manual action)");
    println!("Pattern: Set poison → I/O → disarm on success → check before next op");
    println!("Impact: Converts silent corruption into explicit BrokenPipe error");
    println!("Recovery: Application must reconnect (correct behavior!)");
    println!("\nImplementation: ~170 lines in monocoque-core/src/poison.rs");
    println!("Applied to: All 6 socket types (DEALER, ROUTER, PUB, SUB, REQ, REP)");
    println!("\n=== Demo Complete ===\n");

    Ok(())
}
