use crate::peer_info::{protocol, PeerInfo};
use futures::future::BoxFuture;
use futures::prelude::*;
use libp2p::core::upgrade::{NegotiationError, ReadyUpgrade};
use libp2p::core::UpgradeError;
use libp2p::swarm::handler::{
    ConnectionEvent, DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
    ListenUpgradeError,
};
use libp2p::swarm::{
    ConnectionHandler, ConnectionHandlerEvent, ConnectionHandlerUpgrErr, KeepAlive,
    NegotiatedSubstream, SubstreamProtocol,
};
use std::error::Error;
use std::task::{Context, Poll};
use std::time::Duration;
use std::{fmt, io};
use tracing::{debug, info};

/// The configuration for peer-info protocol.
#[derive(Debug, Clone)]
pub struct Config {
    /// Protocol timeout.
    timeout: Duration,

    /// Protocol name.
    protocol_name: &'static [u8],
}

impl Config {
    /// Creates a new [`Config`] with the following default settings:
    ///
    ///   * [`Config::with_timeout`] 20s
    pub fn new(protocol_name: &'static [u8]) -> Self {
        Self {
            timeout: Duration::from_secs(20),
            protocol_name,
        }
    }

    /// Sets the protocol timeout.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }
}

/// The successful result of processing an inbound or outbound peer info requests.
#[derive(Debug)]
pub enum PeerInfoSuccess {
    Received(PeerInfo),
    DataSent,
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

/// Protocol handler that handles peer-info requests.
///
/// Any protocol failure produces an error that closes the connection.
pub struct Handler {
    /// Configuration options.
    config: Config,
    /// The outbound request state.
    outbound: Option<OutboundState>,
    /// The inbound request future.
    inbound: Option<InPeerInfoFuture>,
    /// Last peer-info error.
    error: Option<PeerInfoError>,
}

impl Handler {
    /// Builds a new [`Handler`] with the given configuration.
    pub fn new(config: Config) -> Self {
        Handler {
            config,
            outbound: None,
            inbound: None,
            error: None,
        }
    }
}

/// Marker struct for outbound peer-info requests.
#[derive(Debug, Clone)]
pub struct HandlerInEvent {
    pub peer_info: PeerInfo,
}

impl ConnectionHandler for Handler {
    type InEvent = HandlerInEvent;
    type OutEvent = super::Result;
    type Error = PeerInfoError;
    type InboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundOpenInfo = PeerInfo;
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.config.protocol_name), ())
    }

    fn on_behaviour_event(&mut self, event: Self::InEvent) {
        if let Some(OutboundState::Idle(stream)) = self.outbound.take() {
            self.outbound = Some(OutboundState::SendingData(
                protocol::send(stream, event.peer_info).boxed(),
            ));
        } else {
            self.outbound = Some(OutboundState::RequestNewStream(event.peer_info));
        }
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        KeepAlive::No
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<
            ReadyUpgrade<&'static [u8]>,
            Self::OutboundOpenInfo,
            super::Result,
            Self::Error,
        >,
    > {
        if let Some(error) = self.error.take() {
            return Poll::Ready(ConnectionHandlerEvent::Close(error));
        }

        // Respond to inbound requests.
        if let Some(fut) = self.inbound.as_mut() {
            match fut.poll_unpin(cx) {
                Poll::Pending => {}
                Poll::Ready(Err(err)) => {
                    debug!(?err, "Peer info handler: inbound peer info error.");

                    return Poll::Ready(ConnectionHandlerEvent::Close(PeerInfoError::Other {
                        error: Box::new(err),
                    }));
                }
                Poll::Ready(Ok((stream, peer_info))) => {
                    info!(?peer_info, "Inbound peer info"); // TODO: downgrade message severity

                    self.inbound = Some(protocol::recv(stream).boxed());
                    return Poll::Ready(ConnectionHandlerEvent::Custom(Ok(
                        PeerInfoSuccess::Received(peer_info),
                    )));
                }
            }
        }

        // Outbound requests.
        match self.outbound.take() {
            Some(OutboundState::SendingData(mut peer_info_fut)) => {
                match peer_info_fut.poll_unpin(cx) {
                    Poll::Pending => {
                        self.outbound = Some(OutboundState::SendingData(peer_info_fut));
                    }
                    Poll::Ready(Ok(stream)) => {
                        self.outbound = Some(OutboundState::Idle(stream));

                        return Poll::Ready(ConnectionHandlerEvent::Custom(Ok(
                            PeerInfoSuccess::DataSent,
                        )));
                    }
                    Poll::Ready(Err(error)) => {
                        debug!(?error, "Outbound peer info error.",);

                        self.error = Some(PeerInfoError::Other {
                            error: Box::new(error),
                        });
                    }
                }
            }
            Some(OutboundState::Idle(stream)) => {
                // Nothing to do but we have a negotiated stream.
                self.outbound = Some(OutboundState::Idle(stream));
            }
            Some(OutboundState::NegotiatingStream) => {
                self.outbound = Some(OutboundState::NegotiatingStream);
            }
            Some(OutboundState::RequestNewStream(peer_info)) => {
                self.outbound = Some(OutboundState::NegotiatingStream);
                let protocol =
                    SubstreamProtocol::new(ReadyUpgrade::new(self.config.protocol_name), peer_info)
                        .with_timeout(self.config.timeout);
                return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { protocol });
            }
            None => {
                // Not initialized yet.
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
                self.inbound = Some(protocol::recv(stream).boxed());
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                info,
            }) => {
                self.outbound = Some(OutboundState::SendingData(
                    protocol::send(stream, info).boxed(),
                ));
            }
            ConnectionEvent::DialUpgradeError(DialUpgradeError { error, .. }) => {
                let error = match error {
                    ConnectionHandlerUpgrErr::Upgrade(UpgradeError::Select(
                        NegotiationError::Failed,
                    )) => PeerInfoError::Unsupported,
                    e => PeerInfoError::Other { error: Box::new(e) },
                };

                self.error = Some(error);
            }
            ConnectionEvent::ListenUpgradeError(ListenUpgradeError { error, .. }) => {
                self.error = Some(PeerInfoError::Other {
                    error: Box::new(error),
                });
            }
            ConnectionEvent::AddressChange(_) => {}
        }
    }
}

type InPeerInfoFuture = BoxFuture<'static, Result<(NegotiatedSubstream, PeerInfo), io::Error>>;
type OutPeerInfoFuture = BoxFuture<'static, Result<NegotiatedSubstream, io::Error>>;

/// The current state w.r.t. outbound peer info requests.
enum OutboundState {
    RequestNewStream(PeerInfo),
    /// A new substream is being negotiated for the protocol.
    NegotiatingStream,
    /// A peer info request is being sent and the response awaited.
    SendingData(OutPeerInfoFuture),
    /// The substream is idle, waiting to send the next peer info request.
    Idle(NegotiatedSubstream),
}
