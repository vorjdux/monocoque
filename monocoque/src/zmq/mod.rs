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

/// The draft socket types (`draft` feature).
#[cfg(feature = "draft")]
pub mod draft {
    pub use monocoque_zmtp::sock::kinds::draft::{
        ChannelSocket, ClientSocket, DishSocket, GatherSocket, MAX_GROUP, RadioSocket,
        ScatterSocket, ServerSocket,
    };
}

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

/// PLAIN and CURVE keys and credentials, and the ZAP handler that decides
/// who may connect.
///
/// A server socket asks the ZAP handler running in its process about every
/// PLAIN client, and about every CURVE client when a handler is running or the
/// socket has a ZAP domain. Start one with [`spawn_zap_server`] before binding.
///
/// [`spawn_zap_server`]: security::spawn_zap_server
pub mod security {
    pub use monocoque_zmtp::security::{
        CurveKeyPair, CurvePublicKey, CurveSecretKey, DefaultZapHandler, PlainAuthHandler,
        PlainCredentials, StaticPlainHandler, ZAP_ENDPOINT, ZapHandler, ZapMechanism, ZapRequest,
        ZapResponse, ZapServer, ZapStatus, spawn_zap_server, start_default_zap_server,
    };

    /// CURVE keys (RFC 26).
    pub mod curve {
        pub use monocoque_zmtp::security::curve::{
            CURVE_KEY_SIZE, CurveKeyPair, CurvePublicKey, CurveSecretKey,
        };
    }

    /// PLAIN credentials and the handler that checks them (RFC 23).
    pub mod plain {
        pub use monocoque_zmtp::security::plain::{
            PlainAuthHandler, PlainCredentials, StaticPlainHandler,
        };
    }

    /// ZAP requests and replies (RFC 27).
    pub mod zap {
        pub use monocoque_zmtp::security::zap::{ZapMechanism, ZapRequest, ZapResponse, ZapStatus};
    }

    /// The ZAP handler trait and the server that runs it.
    pub mod zap_handler {
        pub use monocoque_zmtp::security::zap_handler::{
            DefaultZapHandler, ZapHandler, ZapServer, spawn_zap_server, start_default_zap_server,
        };
    }
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
