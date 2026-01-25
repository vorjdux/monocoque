//! Trait-based socket API for polymorphic socket handling.
//!
//! This module provides a generic `Socket` trait that enables working with
//! different socket types in a uniform way, particularly useful for:
//! - Generic proxy implementations
//! - Testing and mocking
//! - Dynamic socket type selection
//! - Library APIs that work with any socket type

use bytes::Bytes;
use std::io;

use crate::SocketType;

/// Generic socket trait for polymorphic handling of different socket types.
///
/// All ZeroMQ socket types (DEALER, ROUTER, REQ, REP, PAIR, PUSH, PULL, SUB, XSUB, XPUB, PUB)
/// implement this trait, enabling:
/// - Generic functions that work with any socket type
/// - Proxy implementations (e.g., `proxy<F, B>()` where F, B: Socket)
/// - Testing with mock sockets
/// - Runtime socket type selection
///
/// # Examples
///
/// ```no_run
/// use monocoque_zmtp::{Socket, DealerSocket, RouterSocket};
/// use std::io;
///
/// async fn forward_messages<S1, S2>(from: &mut S1, to: &mut S2) -> io::Result<()>
/// where
///     S1: Socket,
///     S2: Socket,
/// {
///     while let Some(msg) = from.recv().await? {
///         to.send(msg).await?;
///     }
///     Ok(())
/// }
/// ```
#[async_trait::async_trait(?Send)]
pub trait Socket {
    /// Send a multipart message on the socket.
    ///
    /// # Arguments
    ///
    /// * `msg` - Multipart message as a vector of frames
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Message sent successfully
    /// - `Err(io::Error)` - Send failed (timeout, disconnection, etc.)
    async fn send(&mut self, msg: Vec<Bytes>) -> io::Result<()>;

    /// Receive a multipart message from the socket.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(msg))` - Message received successfully
    /// - `Ok(None)` - No message available (non-blocking mode)
    /// - `Err(io::Error)` - Receive failed (timeout, disconnection, etc.)
    async fn recv(&mut self) -> io::Result<Option<Vec<Bytes>>>;

    /// Get the socket type.
    ///
    /// Returns the ZeroMQ socket type enum (DEALER, ROUTER, REQ, etc.).
    fn socket_type(&self) -> SocketType;

    /// Check if socket has more message frames pending.
    ///
    /// Equivalent to ZMQ_RCVMORE option. Returns true if the last recv()
    /// operation received a partial message with more frames to follow.
    fn has_more(&self) -> bool {
        // Default implementation - sockets can override if they track this
        false
    }
}

/// Macro to implement the Socket trait for socket types with standard send/recv methods.
///
/// This macro generates boilerplate trait implementations for socket types that follow
/// the standard pattern of having `send(&mut self, Vec<Bytes>)` and `recv(&mut self)` methods.
///
/// # Usage
///
/// ```ignore
/// impl_socket_trait!(DealerSocket<S>, SocketType::Dealer);
/// ```
#[macro_export]
macro_rules! impl_socket_trait {
    ($socket_type:ty, $zmq_type:expr) => {
        #[async_trait::async_trait(?Send)]
        impl<S> $crate::Socket for $socket_type
        where
            S: compio::io::AsyncRead + compio::io::AsyncWrite + Unpin + 'static,
        {
            async fn send(&mut self, msg: Vec<bytes::Bytes>) -> std::io::Result<()> {
                self.send(msg).await
            }

            async fn recv(&mut self) -> std::io::Result<Option<Vec<bytes::Bytes>>> {
                self.recv().await
            }

            fn socket_type(&self) -> $crate::SocketType {
                $zmq_type
            }
        }
    };
}
