use libp2p::core::upgrade::ReadyUpgrade;
use libp2p::swarm::handler::ConnectionEvent;
use libp2p::swarm::{ConnectionHandler, ConnectionHandlerEvent, KeepAlive, SubstreamProtocol};
use std::error::Error;
use std::fmt;
use std::task::{Context, Poll};
use tracing::info;

/// Connection handler for managing connections within our `reserved peers` protocol.
///
/// This `Handler` is part of our custom protocol designed to maintain persistent connections
/// with a set of predefined peers.
///
/// ## Connection Handling
///
/// The `Handler` manages the lifecycle of a connection to each peer. If it's connected to a
/// reserved peer, it maintains the connection alive (`KeepAlive::Yes`). If not, it allows the
/// connection to close (`KeepAlive::No`).
///
/// This behavior ensures that connections to reserved peers are maintained persistently,
/// while connections to non-reserved peers are allowed to close.
pub struct Handler {
    /// Protocol name.
    protocol_name: &'static [u8],

    keep_alive: KeepAlive,
}

impl Handler {
    /// Builds a new [`Handler`].
    pub fn new(protocol_name: &'static [u8], keep_alive: KeepAlive) -> Self {
        Handler {
            protocol_name,
            keep_alive,
        }
    }
}

#[derive(Debug)]
pub struct ReservedPeersError;

impl fmt::Display for ReservedPeersError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Reserved peers error.")
    }
}

impl Error for ReservedPeersError {}

impl ConnectionHandler for Handler {
    type InEvent = KeepAlive;
    type OutEvent = ();
    type Error = ReservedPeersError;
    type InboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundProtocol = ReadyUpgrade<&'static [u8]>;
    type OutboundOpenInfo = ();
    type InboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<ReadyUpgrade<&'static [u8]>, ()> {
        SubstreamProtocol::new(ReadyUpgrade::new(self.protocol_name), ())
    }

    fn on_behaviour_event(&mut self, keep_alive: KeepAlive) {
        info!(?keep_alive, "Behaviour event arrived.");

        self.keep_alive = keep_alive;
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        self.keep_alive
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
