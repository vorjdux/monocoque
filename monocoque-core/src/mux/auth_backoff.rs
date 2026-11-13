//! Slowing down repeated failed authentication from one address.
//!
//! A peer that keeps failing PLAIN or CURVE authorization (wrong password,
//! unknown key) would otherwise get a fresh handshake as fast as it can
//! connect. After [`FREE_FAILURES`] failures in a row from one address, each
//! new connection from it waits before its handshake starts: 100 ms, doubling
//! per further failure, capped at [`MAX_DELAY`]. A success clears the
//! address; one quiet for [`FORGET_AFTER`] is forgotten.
//!
//! Only accepted connections with a peer IP are tracked (IPC and inproc have
//! none), and the table is bounded, so a flood of addresses costs memory only
//! up to [`MAX_ADDRESSES`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Failures from one address before connections from it are delayed.
const FREE_FAILURES: u32 = 2;
const FIRST_DELAY: Duration = Duration::from_millis(100);
const MAX_DELAY: Duration = Duration::from_secs(10);
const FORGET_AFTER: Duration = Duration::from_secs(60);
const MAX_ADDRESSES: usize = 4096;

/// Failure counts by peer address.
#[derive(Default)]
pub struct AuthBackoff {
    failures: HashMap<String, (u32, Instant)>,
}

impl AuthBackoff {
    /// How long a new connection from `address` waits before its handshake.
    pub fn delay(&self, address: &str, now: Instant) -> Duration {
        match self.failures.get(address) {
            Some(&(n, last)) if n > FREE_FAILURES && now - last < FORGET_AFTER => {
                let doublings = (n - FREE_FAILURES - 1).min(16);
                FIRST_DELAY.saturating_mul(1 << doublings).min(MAX_DELAY)
            }
            _ => Duration::ZERO,
        }
    }

    /// A handshake from `address` failed authentication.
    pub fn failed(&mut self, address: &str, now: Instant) {
        if address.is_empty() {
            return;
        }
        if self.failures.len() >= MAX_ADDRESSES && !self.failures.contains_key(address) {
            self.failures
                .retain(|_, (_, last)| now - *last < FORGET_AFTER);
            if self.failures.len() >= MAX_ADDRESSES {
                return;
            }
        }
        let entry = self.failures.entry(address.to_string()).or_insert((0, now));
        if now - entry.1 >= FORGET_AFTER {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    /// A handshake from `address` succeeded.
    pub fn succeeded(&mut self, address: &str) {
        if !self.failures.is_empty() {
            self.failures.remove(address);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_after_the_free_failures_and_success_clears_them() {
        let mut b = AuthBackoff::default();
        let t = Instant::now();
        for _ in 0..FREE_FAILURES {
            b.failed("10.0.0.1", t);
        }
        assert_eq!(b.delay("10.0.0.1", t), Duration::ZERO);
        b.failed("10.0.0.1", t);
        assert_eq!(b.delay("10.0.0.1", t), FIRST_DELAY);
        b.failed("10.0.0.1", t);
        assert_eq!(b.delay("10.0.0.1", t), FIRST_DELAY * 2);
        for _ in 0..40 {
            b.failed("10.0.0.1", t);
        }
        assert_eq!(b.delay("10.0.0.1", t), MAX_DELAY);
        assert_eq!(b.delay("10.0.0.2", t), Duration::ZERO, "per address");
        b.succeeded("10.0.0.1");
        assert_eq!(b.delay("10.0.0.1", t), Duration::ZERO);
    }

    #[test]
    fn quiet_addresses_are_forgotten_and_the_table_is_bounded() {
        let mut b = AuthBackoff::default();
        let t = Instant::now();
        for _ in 0..5 {
            b.failed("10.0.0.1", t);
        }
        assert_eq!(b.delay("10.0.0.1", t + FORGET_AFTER), Duration::ZERO);
        b.failed("10.0.0.1", t + FORGET_AFTER);
        assert_eq!(b.delay("10.0.0.1", t + FORGET_AFTER), Duration::ZERO);
        for i in 0..MAX_ADDRESSES + 100 {
            b.failed(&format!("addr-{i}"), t);
        }
        assert!(b.failures.len() <= MAX_ADDRESSES);
        b.failed("", t);
        assert!(!b.failures.contains_key(""));
    }
}
