//! The ZMTP socket types a peer announces in its READY command.

/// Supported ZMQ socket types (no heap allocation)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketType {
    /// PAIR socket type.
    Pair,
    /// DEALER socket type.
    Dealer,
    /// ROUTER socket type.
    Router,
    /// PUB socket type.
    Pub,
    /// SUB socket type.
    Sub,
    /// REQ socket type.
    Req,
    /// REP socket type.
    Rep,
    /// PUSH socket type.
    Push,
    /// PULL socket type.
    Pull,
    /// XPUB socket type.
    Xpub,
    /// XSUB socket type.
    Xsub,
    /// SERVER socket type (draft).
    Server,
    /// CLIENT socket type (draft).
    Client,
    /// RADIO socket type (draft).
    Radio,
    /// DISH socket type (draft).
    Dish,
    /// SCATTER socket type (draft).
    Scatter,
    /// GATHER socket type (draft).
    Gather,
    /// CHANNEL socket type (draft).
    Channel,
}

impl SocketType {
    /// The socket type a READY command names, if it is one we know.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"PAIR" => Self::Pair,
            b"DEALER" => Self::Dealer,
            b"ROUTER" => Self::Router,
            b"PUB" => Self::Pub,
            b"SUB" => Self::Sub,
            b"REQ" => Self::Req,
            b"REP" => Self::Rep,
            b"PUSH" => Self::Push,
            b"PULL" => Self::Pull,
            b"XPUB" => Self::Xpub,
            b"XSUB" => Self::Xsub,
            b"SERVER" => Self::Server,
            b"CLIENT" => Self::Client,
            b"RADIO" => Self::Radio,
            b"DISH" => Self::Dish,
            b"SCATTER" => Self::Scatter,
            b"GATHER" => Self::Gather,
            b"CHANNEL" => Self::Channel,
            _ => return None,
        })
    }

    /// The draft types exist only in ZMTP 3.1 and carry single-part messages.
    #[must_use]
    pub const fn is_draft(&self) -> bool {
        matches!(
            self,
            Self::Server
                | Self::Client
                | Self::Radio
                | Self::Dish
                | Self::Scatter
                | Self::Gather
                | Self::Channel
        )
    }

    /// Return the wire-format name string for this socket type.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Server => "SERVER",
            Self::Client => "CLIENT",
            Self::Radio => "RADIO",
            Self::Dish => "DISH",
            Self::Scatter => "SCATTER",
            Self::Gather => "GATHER",
            Self::Channel => "CHANNEL",
            Self::Pair => "PAIR",
            Self::Dealer => "DEALER",
            Self::Router => "ROUTER",
            Self::Pub => "PUB",
            Self::Sub => "SUB",
            Self::Req => "REQ",
            Self::Rep => "REP",
            Self::Push => "PUSH",
            Self::Pull => "PULL",
            Self::Xpub => "XPUB",
            Self::Xsub => "XSUB",
        }
    }
}
