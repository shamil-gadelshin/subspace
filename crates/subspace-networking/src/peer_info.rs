mod handler;
mod protocol;

use crate::peer_info::handler::HandlerInEvent;
use handler::Handler;
pub use handler::{Config, PeerInfoError, PeerInfoSuccess};
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::behaviour::{ConnectionEstablished, FromSwarm};
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, NetworkBehaviour, NotifyHandler, PollParameters, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;
use parity_scale_codec::{Decode, Encode};
use std::collections::VecDeque;
use std::task::{Context, Poll};
use tracing::debug;

#[derive(Clone, Debug, Encode, Decode)]
/// Peer info data
pub struct PeerInfo {
    pub role: PeerRole,
}

impl Default for PeerInfo {
    fn default() -> Self {
        PeerInfo {
            role: PeerRole::Client,
        }
    }
}

#[derive(Clone, Debug, Encode, Decode)]
/// Defines the role of a peer
pub enum PeerRole {
    Farmer,
    Node,
    BootstrapNode,
    Client,
}

/// The result of an inbound or outbound peer-info requests.
pub type Result = std::result::Result<PeerInfoSuccess, PeerInfoError>;

/// A [`NetworkBehaviour`] that handles inbound peer info requests and
/// sends outbound peer info requests on the first established connection.
pub struct Behaviour<PeerInfoProvider = ConstantPeerInfoProvider> {
    /// Peer info protocol configuration.
    config: Config,
    /// Queue of events to yield to the swarm.
    events: VecDeque<Event>,
    /// Outbound peer info pushes.
    requests: Vec<Request>,
    /// Provides up-to-date peer info.
    peer_info_provider: PeerInfoProvider,
}

#[derive(Debug, PartialEq, Eq)]
/// Peer info push request.
struct Request {
    peer_id: PeerId,
}

/// Provides the current peer info data.
pub trait PeerInfoProvider: 'static {
    /// Returns the current peer info data.
    fn peer_info(&self) -> PeerInfo;
}

/// Handles constant peer info data.
pub struct ConstantPeerInfoProvider {
    peer_info: PeerInfo,
}

impl ConstantPeerInfoProvider {
    /// Creates a new peer [`ConstantPeerInfoProvider`].
    pub fn new(peer_info: PeerInfo) -> Self {
        Self { peer_info }
    }
}

impl PeerInfoProvider for ConstantPeerInfoProvider {
    fn peer_info(&self) -> PeerInfo {
        self.peer_info.clone()
    }
}

/// Event generated by the `Peer Info` network behaviour.
#[derive(Debug)]
pub struct Event {
    /// The peer ID of the remote.
    pub peer_id: PeerId,
    /// The result of an inbound or outbound peer info request.
    pub result: Result,
}

impl<PIP: PeerInfoProvider> Behaviour<PIP> {
    /// Creates a new `Peer Info` network behaviour with the given configuration.
    pub fn new(config: Config, peer_info_provider: PIP) -> Self {
        Self {
            config,
            peer_info_provider,
            events: VecDeque::new(),
            requests: Vec::new(),
        }
    }
}

impl<PIP: PeerInfoProvider> NetworkBehaviour for Behaviour<PIP> {
    type ConnectionHandler = Handler;
    type OutEvent = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> std::result::Result<THandler<Self>, ConnectionDenied> {
        Ok(Handler::new(self.config.clone()))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
    ) -> std::result::Result<THandler<Self>, ConnectionDenied> {
        Ok(Handler::new(self.config.clone()))
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        _: ConnectionId,
        result: THandlerOutEvent<Self>,
    ) {
        self.events.push_front(Event {
            peer_id: peer,
            result,
        })
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
        _: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::OutEvent, THandlerInEvent<Self>>> {
        if let Some(e) = self.events.pop_back() {
            let Event { result, peer_id } = &e;

            match result {
                Ok(PeerInfoSuccess::DataSent) => {
                    debug!(%peer_id,"Peer info sent.", )
                }
                Ok(PeerInfoSuccess::Received(_)) => {
                    debug!(%peer_id, "Peer info received", )
                }
                Err(err) => {
                    debug!(%peer_id, ?err, "Peer info error");
                }
            }

            return Poll::Ready(ToSwarm::GenerateEvent(e));
        }

        // Check for pending requests.
        if let Some(Request { peer_id }) = self.requests.pop() {
            return Poll::Ready(ToSwarm::NotifyHandler {
                peer_id,
                handler: NotifyHandler::Any,
                event: HandlerInEvent {
                    peer_info: self.peer_info_provider.peer_info(),
                },
            });
        }

        Poll::Pending
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match event {
            FromSwarm::ConnectionEstablished(ConnectionEstablished {
                peer_id,
                other_established,
                ..
            }) => {
                // Push the peer-info request on the first connection.
                if other_established == 0 {
                    self.requests.push(Request { peer_id });
                }
            }
            FromSwarm::ConnectionClosed(_)
            | FromSwarm::AddressChange(_)
            | FromSwarm::DialFailure(_)
            | FromSwarm::ListenFailure(_)
            | FromSwarm::NewListener(_)
            | FromSwarm::NewListenAddr(_)
            | FromSwarm::ExpiredListenAddr(_)
            | FromSwarm::ListenerError(_)
            | FromSwarm::ListenerClosed(_)
            | FromSwarm::NewExternalAddr(_)
            | FromSwarm::ExpiredExternalAddr(_) => {}
        }
    }
}
