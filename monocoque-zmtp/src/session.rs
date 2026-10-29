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
}

impl SocketType {
    /// Return the wire-format name string for this socket type.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
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
