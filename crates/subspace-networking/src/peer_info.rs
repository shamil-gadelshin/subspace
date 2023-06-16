mod handler;

use handler::Handler;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::behaviour::FromSwarm;
use libp2p::swarm::{
    ConnectionDenied, ConnectionId, NetworkBehaviour, PollParameters, THandler, THandlerInEvent,
    THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;
use std::task::{Context, Poll};

// TODO: comments - #![warn(missing_docs)]
// TODO: logs

#[derive(Debug)]
pub struct Behaviour {
    /// Protocol name.
    protocol_name: &'static [u8],
}

// TODO: peerId, addresses
pub struct PeerInfo {
    role: PeerRole
}

pub enum PeerRole {
    Farmer,
    Node,
    BootstrapNode,
    Client,
}

/// Peer-info protocol configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Protocol name.
    pub protocol_name: &'static [u8],
}

#[derive(Debug, Clone)]
pub struct Event;

impl Behaviour {
    pub fn new(config: Config) -> Self {
        Self {
            protocol_name: config.protocol_name,
        }
    }

    /// Create a connection handler for the reserved peers protocol.
    #[inline]
    fn new_peer_info_handler(&self) -> Handler {
        Handler::new(self.protocol_name)
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type OutEvent = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.new_peer_info_handler())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.new_peer_info_handler())
    }

    fn on_swarm_event(&mut self, _event: FromSwarm<Self::ConnectionHandler>) {}

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        _: THandlerOutEvent<Self>,
    ) {
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
        _: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::OutEvent, THandlerInEvent<Self>>> {
        Poll::Pending
    }
}
