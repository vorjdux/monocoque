//! Reconnection utilities with exponential backoff support.
//!
//! This module provides utilities for managing socket reconnection with
//! exponential backoff, following libzmq patterns.

use std::time::Duration;
use crate::options::SocketOptions;

/// Reconnection state tracker for managing connection attempts and backoff.
///
/// This helper tracks the number of reconnection attempts and calculates
/// the appropriate backoff delay using exponential backoff.
///
/// # Example
///
/// ```rust
/// use monocoque_core::reconnect::ReconnectState;
/// use monocoque_core::options::SocketOptions;
/// use std::time::Duration;
///
/// let options = SocketOptions::default()
///     .with_reconnect_ivl(Duration::from_millis(100))
///     .with_reconnect_ivl_max(Duration::from_secs(10));
///
/// let mut reconnect = ReconnectState::new(&options);
///
/// // First attempt uses base interval
/// assert_eq!(reconnect.next_delay(), Duration::from_millis(100));
///
/// // Subsequent attempts use exponential backoff
/// assert_eq!(reconnect.next_delay(), Duration::from_millis(200));
/// assert_eq!(reconnect.next_delay(), Duration::from_millis(400));
///
/// // Reset on successful connection
/// reconnect.reset();
/// assert_eq!(reconnect.next_delay(), Duration::from_millis(100));
/// ```
#[derive(Debug, Clone)]
pub struct ReconnectState {
    /// Base reconnection interval
    base_interval: Duration,
    /// Maximum reconnection interval
    max_interval: Duration,
    /// Current reconnection attempt (0 = first attempt)
    attempt: u32,
    /// Current backoff interval
    current_interval: Duration,
}

impl ReconnectState {
    /// Create a new reconnection state tracker from socket options.
    pub const fn new(options: &SocketOptions) -> Self {
        Self {
            base_interval: options.reconnect_ivl,
            max_interval: options.reconnect_ivl_max,
            attempt: 0,
            current_interval: options.reconnect_ivl,
        }
    }

    /// Get the delay for the next reconnection attempt.
    ///
    /// This calculates the exponential backoff delay based on the number
    /// of previous attempts. The delay doubles with each attempt until
    /// it reaches `reconnect_ivl_max`.
    ///
    /// # Returns
    ///
    /// The duration to wait before the next reconnection attempt.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current_interval;
        
        // Calculate next interval with exponential backoff
        self.attempt += 1;
        self.current_interval = self.base_interval * (1_u32 << self.attempt.min(10));
        
        // Cap at max interval
        if self.current_interval > self.max_interval {
            self.current_interval = self.max_interval;
        }
        
        delay
    }

    /// Reset the reconnection state after a successful connection.
    ///
    /// This resets the attempt counter and interval back to the base values.
    pub fn reset(&mut self) {
        self.attempt = 0;
        self.current_interval = self.base_interval;
    }

    /// Get the current attempt number.
    #[inline]
    #[must_use] 
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Get the base reconnection interval.
    #[inline]
    #[must_use] 
    pub const fn base_interval(&self) -> Duration {
        self.base_interval
    }

    /// Get the maximum reconnection interval.
    #[inline]
    #[must_use] 
    pub const fn max_interval(&self) -> Duration {
        self.max_interval
    }

    /// Get the current reconnection interval.
    #[inline]
    #[must_use] 
    pub const fn current_interval(&self) -> Duration {
        self.current_interval
    }
}

/// Error type for reconnection operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectError {
    /// Maximum reconnection attempts reached
    MaxAttemptsReached { attempts: u32 },
    /// Connection failed with I/O error
    ConnectionFailed { message: String },
    /// Reconnection cancelled by user
    Cancelled,
}

impl std::fmt::Display for ReconnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxAttemptsReached { attempts } => {
                write!(f, "Maximum reconnection attempts reached: {attempts}")
            }
            Self::ConnectionFailed { message } => {
                write!(f, "Connection failed: {message}")
            }
            Self::Cancelled => {
                write!(f, "Reconnection cancelled")
            }
        }
    }
}

impl std::error::Error for ReconnectError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_backoff() {
        let options = SocketOptions::default()
            .with_reconnect_ivl(Duration::from_millis(100))
            .with_reconnect_ivl_max(Duration::from_secs(10));

        let mut state = ReconnectState::new(&options);

        // First attempt: base interval
        assert_eq!(state.next_delay(), Duration::from_millis(100));
        assert_eq!(state.attempt(), 1);

        // Second attempt: doubled
        assert_eq!(state.next_delay(), Duration::from_millis(200));
        assert_eq!(state.attempt(), 2);

        // Third attempt: doubled again
        assert_eq!(state.next_delay(), Duration::from_millis(400));
        assert_eq!(state.attempt(), 3);

        // Fourth attempt
        assert_eq!(state.next_delay(), Duration::from_millis(800));
        assert_eq!(state.attempt(), 4);
    }

    #[test]
    fn test_max_interval_cap() {
        let options = SocketOptions::default()
            .with_reconnect_ivl(Duration::from_millis(100))
            .with_reconnect_ivl_max(Duration::from_millis(500));

        let mut state = ReconnectState::new(&options);

        assert_eq!(state.next_delay(), Duration::from_millis(100));
        assert_eq!(state.next_delay(), Duration::from_millis(200));
        assert_eq!(state.next_delay(), Duration::from_millis(400));
        
        // Should be capped at max
        assert_eq!(state.next_delay(), Duration::from_millis(500));
        assert_eq!(state.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn test_reset() {
        let options = SocketOptions::default()
            .with_reconnect_ivl(Duration::from_millis(100))
            .with_reconnect_ivl_max(Duration::from_secs(10));

        let mut state = ReconnectState::new(&options);

        // Make some attempts
        state.next_delay();
        state.next_delay();
        state.next_delay();
        assert_eq!(state.attempt(), 3);

        // Reset
        state.reset();
        assert_eq!(state.attempt(), 0);
        assert_eq!(state.next_delay(), Duration::from_millis(100));
    }

    #[test]
    fn test_state_accessors() {
        let options = SocketOptions::default()
            .with_reconnect_ivl(Duration::from_millis(250))
            .with_reconnect_ivl_max(Duration::from_secs(5));

        let state = ReconnectState::new(&options);

        assert_eq!(state.base_interval(), Duration::from_millis(250));
        assert_eq!(state.max_interval(), Duration::from_secs(5));
        assert_eq!(state.current_interval(), Duration::from_millis(250));
        assert_eq!(state.attempt(), 0);
    }
}
