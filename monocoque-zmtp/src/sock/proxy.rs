//! `zmq_proxy` and `zmq_proxy_steerable`.
//!
//! The loop waits on readiness ([`Socket::poll_readable`]) and only builds a
//! receive once a message is queued, so no in-flight receive is ever dropped.
//! Forwarding moves the [`Msg`] itself; a capture socket gets a refcounted
//! clone, never a copy of the payload.

use super::kinds::{
    Dealer, Pair, Pub, Pull, Push, Rep, Req, Router, Sub, XPub, XSub,
};
use super::Socket;
use monocoque_core::error::{MonocoqueError, Result};
use monocoque_core::msg::Msg;
use std::future::poll_fn;
use std::task::{Context, Poll};
use tracing::debug;

/// A socket the proxy can forward through.
#[allow(async_fn_in_trait)]
pub trait ProxySocket {
    /// Wait until a receive would complete without waiting.
    fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<()>;
    /// Receive one message.
    async fn recv_msg(&mut self) -> Result<Msg>;
    /// Send one message.
    async fn send_msg(&mut self, msg: Msg) -> Result<()>;
}

macro_rules! proxy_socket {
    (both: $($k:ty),*) => {$(
        impl ProxySocket for Socket<$k> {
            fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<()> {
                Socket::poll_readable(self, cx)
            }
            async fn recv_msg(&mut self) -> Result<Msg> {
                self.recv_owned().await
            }
            async fn send_msg(&mut self, msg: Msg) -> Result<()> {
                self.send(msg).await.map(drop)
            }
        }
    )*};
    (send: $($k:ty),*) => {$(
        impl ProxySocket for Socket<$k> {
            fn poll_readable(&self, _cx: &mut Context<'_>) -> Poll<()> {
                Poll::Pending
            }
            async fn recv_msg(&mut self) -> Result<Msg> {
                Err(MonocoqueError::Unsupported("this socket type cannot receive"))
            }
            async fn send_msg(&mut self, msg: Msg) -> Result<()> {
                self.send(msg).await.map(drop)
            }
        }
    )*};
    (recv: $($k:ty),*) => {$(
        impl ProxySocket for Socket<$k> {
            fn poll_readable(&self, cx: &mut Context<'_>) -> Poll<()> {
                Socket::poll_readable(self, cx)
            }
            async fn recv_msg(&mut self) -> Result<Msg> {
                self.recv_owned().await
            }
            async fn send_msg(&mut self, _msg: Msg) -> Result<()> {
                Err(MonocoqueError::Unsupported("this socket type cannot send"))
            }
        }
    )*};
}

proxy_socket!(both: Pair, Dealer, Router, Req, Rep, XPub, XSub);
proxy_socket!(send: Push, Pub);
proxy_socket!(recv: Pull, Sub);

/// A capture target that never receives anything.
pub struct NoCapture;

impl ProxySocket for NoCapture {
    fn poll_readable(&self, _cx: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
    async fn recv_msg(&mut self) -> Result<Msg> {
        Err(MonocoqueError::Unsupported("no capture socket"))
    }
    async fn send_msg(&mut self, _msg: Msg) -> Result<()> {
        Ok(())
    }
}

/// Control commands for [`proxy_steerable`], sent as one-frame messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyCommand {
    /// Stop forwarding; messages stay queued in the sockets.
    Pause,
    /// Resume forwarding.
    Resume,
    /// Return from the proxy.
    Terminate,
    /// Reply with eight 8-byte frames, as libzmq does: messages in, bytes in,
    /// messages out, bytes out for the frontend, then the same for the backend.
    Statistics,
}

impl ProxyCommand {
    /// Parse a command frame.
    #[must_use]
    pub const fn from_bytes(data: &[u8]) -> Option<Self> {
        match data {
            b"PAUSE" => Some(Self::Pause),
            b"RESUME" => Some(Self::Resume),
            b"TERMINATE" => Some(Self::Terminate),
            b"STATISTICS" => Some(Self::Statistics),
            _ => None,
        }
    }

    /// The command's wire form.
    #[must_use]
    pub const fn as_bytes(&self) -> &'static [u8] {
        match self {
            Self::Pause => b"PAUSE",
            Self::Resume => b"RESUME",
            Self::Terminate => b"TERMINATE",
            Self::Statistics => b"STATISTICS",
        }
    }
}

#[derive(Default)]
struct SideStats {
    msgs_in: u64,
    bytes_in: u64,
    msgs_out: u64,
    bytes_out: u64,
}

#[derive(Clone, Copy)]
enum Ready {
    Control,
    Frontend,
    Backend,
}

/// A send error that drops the message but keeps the proxy running.
const fn is_transient(err: &MonocoqueError) -> bool {
    matches!(
        err,
        MonocoqueError::Again | MonocoqueError::TimedOut | MonocoqueError::HostUnreachable
    )
}

async fn forward<S: ProxySocket, D: ProxySocket, C: ProxySocket>(
    src: &mut S,
    dst: &mut D,
    capture: &mut Option<&mut C>,
    stats: (&mut SideStats, &mut SideStats),
) -> Result<()> {
    let msg = src.recv_msg().await?;
    let len = msg.byte_len() as u64;
    stats.0.msgs_in += 1;
    stats.0.bytes_in += len;
    if let Some(cap) = capture
        && let Err(e) = cap.send_msg(msg.clone()).await
    {
        debug!("proxy capture send failed: {e}");
    }
    match dst.send_msg(msg).await {
        Ok(()) => {
            stats.1.msgs_out += 1;
            stats.1.bytes_out += len;
            Ok(())
        }
        Err(e) if is_transient(&e) => {
            debug!("proxy dropped a message: {e}");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Forward messages between `frontend` and `backend` until an error.
///
/// Like `zmq_proxy`: every message from one side is sent to the other, and a
/// copy goes to `capture` if one is given.
pub async fn proxy<F, B, C>(
    frontend: &mut F,
    backend: &mut B,
    capture: Option<&mut C>,
) -> Result<()>
where
    F: ProxySocket,
    B: ProxySocket,
    C: ProxySocket,
{
    run(frontend, backend, capture, None::<&mut NoCapture>).await
}

/// [`proxy`] steered by [`ProxyCommand`]s arriving on `control`.
pub async fn proxy_steerable<F, B, C, K>(
    frontend: &mut F,
    backend: &mut B,
    capture: Option<&mut C>,
    control: &mut K,
) -> Result<()>
where
    F: ProxySocket,
    B: ProxySocket,
    C: ProxySocket,
    K: ProxySocket,
{
    run(frontend, backend, capture, Some(control)).await
}

async fn run<F, B, C, K>(
    frontend: &mut F,
    backend: &mut B,
    mut capture: Option<&mut C>,
    mut control: Option<&mut K>,
) -> Result<()>
where
    F: ProxySocket,
    B: ProxySocket,
    C: ProxySocket,
    K: ProxySocket,
{
    let mut paused = false;
    let mut front = SideStats::default();
    let mut back = SideStats::default();
    // Alternate which side is checked first so neither starves the other.
    let mut front_first = true;
    loop {
        let ready = poll_fn(|cx| {
            if let Some(ctl) = control.as_deref()
                && ctl.poll_readable(cx).is_ready()
            {
                return Poll::Ready(Ready::Control);
            }
            if paused {
                return Poll::Pending;
            }
            // When neither side is ready both have been polled, so both
            // will wake this task.
            let order = if front_first {
                [Ready::Frontend, Ready::Backend]
            } else {
                [Ready::Backend, Ready::Frontend]
            };
            for side in order {
                let readable = match side {
                    Ready::Frontend => frontend.poll_readable(cx),
                    _ => backend.poll_readable(cx),
                };
                if readable.is_ready() {
                    return Poll::Ready(side);
                }
            }
            Poll::Pending
        })
        .await;
        front_first = !front_first;
        match ready {
            Ready::Frontend => forward(frontend, backend, &mut capture, (&mut front, &mut back)).await?,
            Ready::Backend => forward(backend, frontend, &mut capture, (&mut back, &mut front)).await?,
            Ready::Control => {
                let Some(ctl) = control.as_deref_mut() else {
                    continue;
                };
                let cmd = ctl.recv_msg().await?;
                match cmd.frames().next().and_then(ProxyCommand::from_bytes) {
                    Some(ProxyCommand::Pause) => paused = true,
                    Some(ProxyCommand::Resume) => paused = false,
                    Some(ProxyCommand::Terminate) => return Ok(()),
                    Some(ProxyCommand::Statistics) => {
                        let reply: Msg = [&front, &back]
                            .into_iter()
                            .flat_map(|s| [s.msgs_in, s.bytes_in, s.msgs_out, s.bytes_out])
                            .map(|n| bytes::Bytes::copy_from_slice(&n.to_le_bytes()))
                            .collect();
                        let _ = ctl.send_msg(reply).await;
                    }
                    None => debug!("proxy ignored an unknown control command"),
                }
            }
        }
    }
}
