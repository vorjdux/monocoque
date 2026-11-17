//! Probe for counting atomic instructions per message under callgrind; see
//! `scripts/count_atomics.sh`, which builds and runs it. This is tooling for
//! that measurement, not a usage example.
//!
//! ```text
//! atomics_probe roundtrip <n>        PUSH -> PULL over TCP, one message at a time
//! atomics_probe queued <n> <take>    queue n messages, then receive `take` of them
//! ```
//!
//! Two runs that differ only in the count isolate the per-message cost from
//! setup. `queued` with take = 0 and take = n isolates the borrowed receive of
//! a message already in the queue, which is meant to cost no atomics at all.

use monocoque::zmq::{Msg, PullSocket, PushSocket, SocketOptions};
use std::time::Duration;

fn usage() -> ! {
    eprintln!("Usage:");
    eprintln!("  atomics_probe roundtrip <n>");
    eprintln!("  atomics_probe queued <n> [take]   (take defaults to n)");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(mode), Some(Ok(n))) = (
        args.get(1).cloned(),
        args.get(2).map(|s| s.parse::<usize>()),
    ) else {
        usage();
    };
    let take = match args.get(3).map(|s| s.parse::<usize>()) {
        None => n,
        Some(Ok(take)) => take,
        Some(Err(_)) => usage(),
    };
    monocoque::rt::LocalRuntime::new()
        .unwrap()
        .block_on(async move {
            let mut pull = PullSocket::with_options(SocketOptions::default().with_recv_hwm(0));
            let ep = pull.bind("tcp://127.0.0.1:0").await.unwrap();
            let mut push = PushSocket::with_options(SocketOptions::default().with_send_hwm(0));
            push.connect(&ep).await.unwrap();
            let msg = Msg::from(vec![7u8; 64]);
            push.send(msg.clone()).await.unwrap();
            pull.recv().await.unwrap();
            if mode == "roundtrip" {
                for _ in 0..n {
                    push.send(msg.clone()).await.unwrap();
                    std::hint::black_box(pull.recv().await.unwrap().len());
                }
                return;
            }
            for _ in 0..n {
                push.send(msg.clone()).await.unwrap();
            }
            push.flush().await.unwrap();
            // Generous even under valgrind: every message is in the receive
            // queue before the measured receives start.
            monocoque::rt::sleep(Duration::from_secs(4)).await;
            for _ in 0..take {
                std::hint::black_box(pull.recv().await.unwrap().len());
            }
        });
}
