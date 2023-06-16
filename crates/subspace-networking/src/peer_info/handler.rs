use libp2p::core::upgrade::ReadyUpgrade;
use libp2p::swarm::handler::ConnectionEvent;
use libp2p::swarm::{ConnectionHandler, ConnectionHandlerEvent, KeepAlive, SubstreamProtocol};
use std::error::Error;
use std::fmt;
use std::task::{Context, Poll};
use void::Void;

// TODO: comments - #![warn(missing_docs)]
// TODO: logs

pub struct Handler {
    /// Protocol name.
    protocol_name: &'static [u8],
}

impl Handler {
    /// Builds a new [`Handler`].
    pub fn new(protocol_name: &'static [u8]) -> Self {
        Handler { protocol_name }
    }
}

#[derive(Debug)]
pub struct PeerInfoError;

impl fmt::Display for PeerInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Peer info error.")
    }
}

impl Error for PeerInfoError {}

impl ConnectionHandler for Handler {
    type InEvent = Void;
    type OutEvent = ();
    type Error = PeerInfoError;
    type InboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundOpenInfo = ();
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name), ())
    }

    fn on_behaviour_event(&mut self, _: Void) {}

    fn connection_keep_alive(&self) -> KeepAlive {
        //TODO:
        KeepAlive::No
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<ReadyUpgrade<&'static [u8]>, (), (), Self::Error>> {
        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        _: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
    }
}
