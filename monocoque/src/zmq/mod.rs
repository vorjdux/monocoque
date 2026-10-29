//! ZeroMQ sockets.
//!
//! Every socket is created unconnected, then bound and/or connected to any
//! number of `tcp://`, `ipc://` and `inproc://` endpoints. Messages are
//! [`Msg`]: a single small frame is stored inline, larger frames are shared
//! `Bytes`, so neither sending nor receiving copies a payload. A receive lends
//! the message out as a [`MsgRef`]; call [`MsgRef::take`] or `recv_owned` to
//! keep it. Every operation returns [`Result`] with a [`MonocoqueError`].
//!
//! # Socket types
//!
//! - [`DealerSocket`], [`RouterSocket`]: asynchronous request/reply
//! - [`ReqSocket`], [`RepSocket`]: strict request/reply
//! - [`PubSocket`], [`SubSocket`], [`XPubSocket`], [`XSubSocket`]: publish/subscribe
//! - [`PushSocket`], [`PullSocket`]: pipelines
//! - [`PairSocket`]: one exclusive peer
//!
//! # Example
//!
//! ```rust,no_run
//! use monocoque::zmq::{RepSocket, ReqSocket};
//!
//! # async fn example() -> monocoque::zmq::Result<()> {
//! let mut rep = RepSocket::new();
//! let endpoint = rep.bind("tcp://127.0.0.1:0").await?;
//!
//! let mut req = ReqSocket::new();
//! req.connect(&endpoint).await?;
//! req.send("hello").await?;
//!
//! let request = rep.recv_owned().await?;
//! rep.send(request).await?;
//! assert_eq!(req.recv().await?, [b"hello"]);
//! # Ok(())
//! # }
//! ```

pub use monocoque_core::endpoint::{Endpoint, EndpointError};
pub use monocoque_core::error::{MonocoqueError, Result};
pub use monocoque_core::monitor::{SocketEvent, SocketMonitor};
pub use monocoque_core::msg::{Metadata, Msg, MsgRef};
pub use monocoque_core::mux::{PeerId, PeerInfo};
pub use monocoque_core::options::SocketOptions;
pub use monocoque_core::subscription::{Subscription, SubscriptionEvent, SubscriptionTrie};
pub use monocoque_zmtp::session::SocketType;
pub use monocoque_zmtp::sock::Socket;
pub use monocoque_zmtp::sock::kinds::{
    DealerSocket, PairSocket, PubSocket, PullSocket, PushSocket, RepSocket, ReqSocket,
    RouterSocket, SubSocket, XPubSocket, XSubSocket,
};
pub use monocoque_zmtp::sock::stream::StreamSocket;

/// `zmq_poll`: wait on many sockets of any kind at once.
pub mod poll {
    pub use monocoque_zmtp::sock::poll::{PollEvents, PollItem, Pollable, poll};
}

/// `zmq_proxy` and `zmq_proxy_steerable`.
pub mod proxy {
    pub use monocoque_zmtp::sock::proxy::{
        NoCapture, ProxyCommand, ProxySocket, proxy, proxy_steerable,
    };
}

#[cfg(unix)]
pub use monocoque_core::ipc;

/// Everything needed to write a socket program.
pub mod prelude {
    pub use super::{
        DealerSocket, MonocoqueError, Msg, MsgRef, PairSocket, PubSocket, PullSocket, PushSocket,
        RepSocket, ReqSocket, Result, RouterSocket, SocketOptions, SocketType, SubSocket,
        XPubSocket, XSubSocket,
    };
    pub use bytes::Bytes;
}
