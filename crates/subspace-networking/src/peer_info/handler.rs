use crate::peer_info::{protocol, PeerInfo, PROTOCOL_NAME};
use futures::future::BoxFuture;
use futures::prelude::*;
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
use std::task::{Context, Poll};
use std::time::Duration;
use std::{fmt, io};
use std::error::Error;
use tracing::{debug, info};
use void::Void;

// TODO: comments - #![warn(missing_docs)]
// TODO: logs

/// The configuration for outbound pings.
#[derive(Debug, Clone)]
pub struct Config {
    /// The timeout of an outbound ping.
    timeout: Duration,
    peer_info: PeerInfo,
}

impl Config {
    /// Creates a new [`Config`] with the following default settings:
    ///
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
            peer_info: PeerInfo::default(),
        }
    }

    /// Sets the protocol timeout.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    pub fn with_peer_info(mut self, pi: PeerInfo) -> Self {
        self.peer_info = pi;
        self
    }
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

/// A peer info protocol failure.
#[derive(Debug)]
pub enum PeerInfoError {
    /// The peer does not support the peer info protocol.
    Unsupported,
    /// The peer info request failed.
    Other {
        error: Box<dyn Error + Send + 'static>,
    },
}

impl fmt::Display for PeerInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerInfoError::Other { error } => write!(f, "Peer info error: {error}"),
            PeerInfoError::Unsupported => write!(f, "Peer info protocol not supported"),
        }
    }
}

impl Error for PeerInfoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            PeerInfoError::Other { error } => Some(&**error),
            PeerInfoError::Unsupported => None,
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
    /// Outbound ping failures that are pending to be processed by `poll()`.
    pending_errors: VecDeque<PeerInfoError>,
    /// The outbound ping state.
    outbound: Option<OutboundState>,
    /// The inbound pong handler, i.e. if there is an inbound
    /// substream, this is always a future that waits for the
    /// next inbound ping to be answered.
    inbound: Option<PeerInfoFuture>,
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
            e => PeerInfoError::Other { error: Box::new(e) },
        };

        self.pending_errors.push_front(error);
    }
}

impl ConnectionHandler for Handler {
    type InEvent = Void;
    type OutEvent = super::Result;
    type Error = PeerInfoError;
    type InboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundOpenInfo = ();
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(PROTOCOL_NAME), ())
    }

    fn on_behaviour_event(&mut self, _: Void) {}

    fn connection_keep_alive(&self) -> KeepAlive {
        KeepAlive::No
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<ReadyUpgrade<&'static [u8]>, (), super::Result, Self::Error>>
    {
        // // Check for outbound ping failures.
        // if let Some(error) = self.pending_errors.pop_back() {
        //     log::debug!("Ping failure: {:?}", error);
        //
        //     self.failures += 1;
        //
        //     // Note: For backward-compatibility, with configured
        //     // `max_failures == 1`, the first failure is always "free"
        //     // and silent. This allows peers who still use a new substream
        //     // for each ping to have successful ping exchanges with peers
        //     // that use a single substream, since every successful ping
        //     // resets `failures` to `0`, while at the same time emitting
        //     // events only for `max_failures - 1` failures, as before.
        //     if self.failures > 1 || self.config.max_failures.get() > 1 {
        //         if self.failures >= self.config.max_failures.get() {
        //             log::debug!("Too many failures ({}). Closing connection.", self.failures);
        //             return Poll::Ready(ConnectionHandlerEvent::Close(error));
        //         }
        //
        //         return Poll::Ready(ConnectionHandlerEvent::Custom(Err(error)));
        //     }
        // }


        match self.state {
            State::Inactive { reported: true } => {
                return Poll::Pending; // nothing to do on this connection
            }
            State::Inactive { reported: false } => {
                self.state = State::Inactive { reported: true };
                return Poll::Ready(ConnectionHandlerEvent::Custom(Err(PeerInfoError::Unsupported)));
            }
            State::Active => {}
        }

        // Respond to inbound pings.
        if let Some(fut) = self.inbound.as_mut() {
            match fut.poll_unpin(cx) {
                Poll::Pending => {}
                Poll::Ready(Err(e)) => {
                    debug!("Inbound peer info error: {:?}", e);
                    self.inbound = None; // TODO:
                }
                Poll::Ready(Ok((stream, peer_info))) => {
                    info!(?peer_info, "Inbound peer info"); // TODO:
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
                Some(OutboundState::PeerInfo(mut ping)) => match ping.poll_unpin(cx) {
                    Poll::Pending => {
                        self.outbound = Some(OutboundState::PeerInfo(ping));
                        break;
                    }
                    Poll::Ready(Ok((stream, peer_info))) => {
                        info!(?peer_info, "Outbound peer info"); // TODO:
                        self.outbound = Some(OutboundState::Idle(stream));
                        return Poll::Ready(ConnectionHandlerEvent::Custom(Ok(Success::Ping {})));
                    }
                    Poll::Ready(Err(e)) => {
                        self.pending_errors
                            .push_front(PeerInfoError::Other { error: Box::new(e) });
                    }
                },
                Some(OutboundState::Idle(stream)) =>  {
                    self.outbound = Some(OutboundState::Idle(stream));
                    break;
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
                self.outbound = Some(OutboundState::PeerInfo(
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

type PeerInfoFuture = BoxFuture<'static, Result<(NegotiatedSubstream, PeerInfo), io::Error>>;

/// The current state w.r.t. outbound peer info requests.
enum OutboundState {
    /// A new substream is being negotiated for the protocol.
    OpenStream,
    /// The substream is idle, waiting to send the next peer info request.
    Idle(NegotiatedSubstream),
    /// A peer info request is being sent and the response awaited.
    PeerInfo(PeerInfoFuture),
}
