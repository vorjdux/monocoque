//! Probe: does `rt::sleep` still hang after `rt::timeout` expirations?
//!
//! On compio 0.10 several `rt::timeout` expirations left residual timer state
//! that made a subsequent `rt::sleep` hang forever (the reason the reconnect
//! path used a blocking `std::thread::sleep`). This checks the current backend.

use monocoque_core::rt::{LocalRuntime, sleep, timeout};
use std::time::{Duration, Instant};

#[test]
fn sleep_completes_after_multiple_timeout_expirations() {
    LocalRuntime::new().unwrap().block_on(async {
        // Reproduce the original trigger: several timed-out operations.
        for _ in 0..5 {
            let never = std::future::pending::<()>();
            let _ = timeout(Duration::from_millis(15), never).await;
        }

        // This is the call that used to hang. Guard the whole test with a
        // timeout so a regression fails fast instead of hanging CI.
        let start = Instant::now();
        let slept = timeout(Duration::from_secs(5), sleep(Duration::from_millis(50))).await;
        assert!(
            slept.is_ok(),
            "rt::sleep hung after timeout expirations (compio timer regression)"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(40),
            "rt::sleep returned too early"
        );
    });
}
