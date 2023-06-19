use crate::peer_info::{protocol, PeerInfo, PROTOCOL_NAME};
use futures::future::BoxFuture;
use futures::prelude::*;
use futures_timer::Delay;
use libp2p::core::upgrade::{NegotiationError, ReadyUpgrade};
use libp2p::core::UpgradeError;
use libp2p::swarm::handler::{
    ConnectionEvent, DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
};
use libp2p::swarm::{
    ConnectionHandler, ConnectionHandlerEvent, ConnectionHandlerUpgrErr, KeepAlive,
    NegotiatedSubstream, SubstreamProtocol,
};
use std::collections::VecDeque;
use std::error::Error;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{fmt, io};
use tracing::{debug, info};
use void::Void;

// TODO: comments - #![warn(missing_docs)]
// TODO: logs

// pub struct Handler {
//     /// Protocol name.
//     protocol_name: &'static [u8],
// }
//
// impl Handler {
//     /// Builds a new [`Handler`].
//     pub fn new(protocol_name: &'static [u8]) -> Self {
//         Handler { protocol_name }
//     }
// }

#[derive(Debug)]
pub struct PeerInfoError;

impl fmt::Display for PeerInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Peer info error.")
    }
}

impl Error for PeerInfoError {}

// impl ConnectionHandler for Handler {
//     type InEvent = Void;
//     type OutEvent = ();
//     type Error = PeerInfoError;
//     type InboundProtocol = ReadyUpgrade<&'static [u8]>;
//     type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
//     type OutboundOpenInfo = ();
//     type InboundOpenInfo = ();
//
//     fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
//         SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name), ())
//     }
//
//     fn on_behaviour_event(&mut self, _: Void) {}
//
//     fn connection_keep_alive(&self) -> KeepAlive {
//         //TODO:
//         KeepAlive::No
//     }
//
//     fn poll(
//         &mut self,
//         _: &mut Context<'_>,
//     ) -> Poll<ConnectionHandlerEvent<ReadyUpgrade<&'static [u8]>, (), (), Self::Error>> {
//         Poll::Pending
//     }
//
//     fn on_connection_event(
//         &mut self,
//         _: ConnectionEvent<
//             Self::InboundProtocol,
//             Self::OutboundProtocol,
//             Self::InboundOpenInfo,
//             Self::OutboundOpenInfo,
//         >,
//     ) {
//     }
// }

/// The configuration for outbound pings.
#[derive(Debug, Clone)]
pub struct Config {
    /// The timeout of an outbound ping.
    timeout: Duration,
    /// The duration between the last successful outbound or inbound ping
    /// and the next outbound ping.
    interval: Duration,
    /// Whether the connection should generally be kept alive.
    keep_alive: bool,

    peer_info: PeerInfo,
}

impl Config {
    /// Creates a new [`Config`] with the following default settings:
    ///
    ///   * [`Config::with_interval`] 15s
    ///   * [`Config::with_timeout`] 20s
    ///   * [`Config::with_max_failures`] 1
    ///   * [`Config::with_keep_alive`] false
    ///
    /// These settings have the following effect:
    ///
    ///   * A ping is sent every 15 seconds on a healthy connection.
    ///   * Every ping sent must yield a response within 20 seconds in order to
    ///     be successful.
    ///   * A single ping failure is sufficient for the connection to be subject
    ///     to being closed.
    ///   * The connection may be closed at any time as far as the ping protocol
    ///     is concerned, i.e. the ping protocol itself does not keep the
    ///     connection alive.
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(20),
            interval: Duration::from_secs(15),
            keep_alive: false,
            peer_info: PeerInfo::default(),
        }
    }

    /// Sets the ping timeout.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    /// Sets the ping interval.
    pub fn with_interval(mut self, d: Duration) -> Self {
        self.interval = d;
        self
    }

    pub fn with_peer_info(mut self, pi: PeerInfo) -> Self {
        self.peer_info = pi;
        self
    }

    // /// Sets whether the ping protocol itself should keep the connection alive,
    // /// apart from the maximum allowed failures.
    // ///
    // /// By default, the ping protocol itself allows the connection to be closed
    // /// at any time, i.e. in the absence of ping failures the connection lifetime
    // /// is determined by other protocol handlers.
    // ///
    // /// If the maximum number of allowed ping failures is reached, the
    // /// connection is always terminated as a result of [`ConnectionHandler::poll`]
    // /// returning an error, regardless of the keep-alive setting.
    // #[deprecated(
    //     since = "0.40.0",
    //     note = "Use `libp2p::swarm::behaviour::KeepAlive` if you need to keep connections alive unconditionally."
    // )]
    // pub fn with_keep_alive(mut self, b: bool) -> Self {
    //     self.keep_alive = b;
    //     self
    // }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// The successful result of processing an inbound or outbound ping.
#[derive(Debug)]
pub enum Success {
    /// Received a ping and sent back a pong.
    Pong,
    /// Sent a ping and received back a pong.
    ///
    /// Includes the round-trip time.
    Ping,
}

/// An outbound ping failure.
#[derive(Debug)]
pub enum Failure {
    /// The ping timed out, i.e. no response was received within the
    /// configured ping timeout.
    Timeout,
    /// The peer does not support the ping protocol.
    Unsupported,
    /// The ping failed for reasons other than a timeout.
    Other {
        error: Box<dyn std::error::Error + Send + 'static>,
    },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Timeout => f.write_str("Ping timeout"),
            Failure::Other { error } => write!(f, "Ping error: {error}"),
            Failure::Unsupported => write!(f, "Ping protocol not supported"),
        }
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Failure::Timeout => None,
            Failure::Other { error } => Some(&**error),
            Failure::Unsupported => None,
        }
    }
}

/// Protocol handler that handles pinging the remote at a regular period
/// and answering ping queries.
///
/// If the remote doesn't respond, produces an error that closes the connection.
pub struct Handler {
    /// Configuration options.
    config: Config,
    /// The timer used for the delay to the next ping as well as
    /// the ping timeout.
    timer: Delay,
    /// Outbound ping failures that are pending to be processed by `poll()`.
    pending_errors: VecDeque<Failure>,
    /// The outbound ping state.
    outbound: Option<OutboundState>,
    /// The inbound pong handler, i.e. if there is an inbound
    /// substream, this is always a future that waits for the
    /// next inbound ping to be answered.
    inbound: Option<PingFuture>,
    /// Tracks the state of our handler.
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// We are inactive because the other peer doesn't support ping.
    Inactive {
        /// Whether or not we've reported the missing support yet.
        ///
        /// This is used to avoid repeated events being emitted for a specific connection.
        reported: bool,
    },
    /// We are actively pinging the other peer.
    Active,
}

impl Handler {
    /// Builds a new [`Handler`] with the given configuration.
    pub fn new(config: Config) -> Self {
        Handler {
            config,
            timer: Delay::new(Duration::new(0, 0)),
            pending_errors: VecDeque::with_capacity(2),
            outbound: None,
            inbound: None,
            state: State::Active,
        }
    }

    fn on_dial_upgrade_error(
        &mut self,
        DialUpgradeError { error, .. }: DialUpgradeError<
            <Self as ConnectionHandler>::OutboundOpenInfo,
            <Self as ConnectionHandler>::OutboundProtocol,
        >,
    ) {
        self.outbound = None; // Request a new substream on the next `poll`.

        let error = match error {
            ConnectionHandlerUpgrErr::Upgrade(UpgradeError::Select(NegotiationError::Failed)) => {
                debug_assert_eq!(self.state, State::Active);

                self.state = State::Inactive { reported: false };
                return;
            }
            // Note: This timeout only covers protocol negotiation.
            ConnectionHandlerUpgrErr::Timeout => Failure::Timeout,
            e => Failure::Other { error: Box::new(e) },
        };

        self.pending_errors.push_front(error);
    }
}

impl ConnectionHandler for Handler {
    type InEvent = Void;
    type OutEvent = super::Result;
    type Error = Failure;
    type InboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundOpenInfo = ();
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(PROTOCOL_NAME), ())
    }

    fn on_behaviour_event(&mut self, _: Void) {}

    fn connection_keep_alive(&self) -> KeepAlive {
        // if self.config.keep_alive {
        //     KeepAlive::Yes
        // } else {
        //     KeepAlive::No
        // }
        KeepAlive::Yes
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<ReadyUpgrade<&'static [u8]>, (), super::Result, Self::Error>>
    {
        match self.state {
            State::Inactive { reported: true } => {
                return Poll::Pending; // nothing to do on this connection
            }
            State::Inactive { reported: false } => {
                self.state = State::Inactive { reported: true };
                return Poll::Ready(ConnectionHandlerEvent::Custom(Err(Failure::Unsupported)));
            }
            State::Active => {}
        }

        // Respond to inbound pings.
        if let Some(fut) = self.inbound.as_mut() {
            match fut.poll_unpin(cx) {
                Poll::Pending => {}
                Poll::Ready(Err(e)) => {
                    debug!("Inbound ping error: {:?}", e);
                    self.inbound = None;
                }
                Poll::Ready(Ok((stream, peer_info))) => {
                    info!(?peer_info, "Inbound peer info");
                    // A ping from a remote peer has been answered, wait for the next.
                    self.inbound =
                        Some(protocol::recv(stream, self.config.peer_info.clone()).boxed());
                    return Poll::Ready(ConnectionHandlerEvent::Custom(Ok(Success::Pong)));
                }
            }
        }

        loop {
            // Continue outbound pings.
            match self.outbound.take() {
                Some(OutboundState::Ping(mut ping)) => match ping.poll_unpin(cx) {
                    Poll::Pending => {
                        if self.timer.poll_unpin(cx).is_ready() {
                            self.pending_errors.push_front(Failure::Timeout);
                        } else {
                            self.outbound = Some(OutboundState::Ping(ping));
                            break;
                        }
                    }
                    Poll::Ready(Ok((stream, peer_info))) => {
                        info!(?peer_info, "Outbound peer info");
                        self.timer.reset(self.config.interval);
                        self.outbound = Some(OutboundState::Idle(stream));
                        return Poll::Ready(ConnectionHandlerEvent::Custom(Ok(Success::Ping {})));
                    }
                    Poll::Ready(Err(e)) => {
                        self.pending_errors
                            .push_front(Failure::Other { error: Box::new(e) });
                    }
                },
                Some(OutboundState::Idle(stream)) => match self.timer.poll_unpin(cx) {
                    Poll::Pending => {
                        self.outbound = Some(OutboundState::Idle(stream));
                        break;
                    }
                    Poll::Ready(()) => {
                        self.timer.reset(self.config.timeout);
                        self.outbound = Some(OutboundState::Ping(
                            protocol::send(stream, self.config.peer_info.clone()).boxed(),
                        ));
                    }
                },
                Some(OutboundState::OpenStream) => {
                    self.outbound = Some(OutboundState::OpenStream);
                    break;
                }
                None => {
                    self.outbound = Some(OutboundState::OpenStream);
                    let protocol = SubstreamProtocol::new(ReadyUpgrade::new(PROTOCOL_NAME), ())
                        .with_timeout(self.config.timeout);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol,
                    });
                }
            }
        }

        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: stream,
                ..
            }) => {
                self.inbound = Some(protocol::recv(stream, self.config.peer_info.clone()).boxed());
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                self.timer.reset(self.config.timeout);
                self.outbound = Some(OutboundState::Ping(
                    protocol::send(stream, self.config.peer_info.clone()).boxed(),
                ));
            }
            ConnectionEvent::DialUpgradeError(dial_upgrade_error) => {
                self.on_dial_upgrade_error(dial_upgrade_error)
            }
            ConnectionEvent::AddressChange(_) | ConnectionEvent::ListenUpgradeError(_) => {}
        }
    }
}

type PingFuture = BoxFuture<'static, Result<(NegotiatedSubstream, PeerInfo), io::Error>>;

/// The current state w.r.t. outbound pings.
enum OutboundState {
    /// A new substream is being negotiated for the ping protocol.
    OpenStream,
    /// The substream is idle, waiting to send the next ping.
    Idle(NegotiatedSubstream),
    /// A ping is being sent and the response awaited.
    Ping(PingFuture),
}
