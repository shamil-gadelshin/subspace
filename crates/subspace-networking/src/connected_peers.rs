mod handler;

use handler::Handler;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::behaviour::{ConnectionEstablished, FromSwarm};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    ConnectionClosed, ConnectionDenied, ConnectionId, DialFailure, NetworkBehaviour,
    PollParameters, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;
use std::collections::HashMap;
use std::ops::Add;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tracing::{debug, trace, warn};

use crate::utils::{convert_multiaddresses, PeerAddress};

pub trait PeerAddressSource {
    fn peer_addresses(&self, batch_size: u32) -> Vec<PeerAddress>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PeerDecision {
    PendingConnection { state: PeerState },
    PendingDecision,
    PermanentConnection,
    NotInterested,
}

/// Reserved peers protocol configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Protocol name.
    pub protocol_name: &'static [u8],

    pub dialing_interval: Duration,

    pub target_connected_peers: u32,
    pub next_peer_batch_size: u32,
}

/// Reserved peer connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Reserved peer is not connected. The next connection attempt is scheduled.
    NotConnected { scheduled_at: Instant },
    /// Reserved peer dialing is in progress.
    PendingConnection,
    /// Reserved peer is connected.
    Connected,
}

/// We pause between reserved peers dialing otherwise we could do multiple dials to offline peers
/// wasting resources and producing a ton of log records.
const DIALING_INTERVAL_IN_SECS: Duration = Duration::from_secs(1);

/// Helper-function to schedule a connection attempt.
#[inline]
fn schedule_connection() -> Instant {
    Instant::now().add(DIALING_INTERVAL_IN_SECS)
}

/// Reserved peer connection events.
/// Initially the "reserved peers behaviour" doesn't produce events. However, we could pass
/// reserved peer state changes to the swarm using this struct in the future.
#[derive(Debug, Clone)]
pub struct Event;

/// Defines the state of a reserved peer connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerState {
    connection_status: ConnectionStatus,
    peer_id: PeerId,
    address: Multiaddr,
}

/// `Behaviour` controls and maintains the state of connections to a predefined set of peers.
///
/// The `Behaviour` struct is part of our custom protocol that aims to maintain persistent
/// connections to a predefined set of peers. It encapsulates the logic of managing the connections,
/// dialing, and handling various states of these connections.
///
/// ## How it works
///
/// Each `ReservedPeerState` can be in one of the following states, represented by the
/// `ConnectionStatus` enum:
/// 1. `NotConnected`: This state indicates that the peer is currently not connected.
/// The time for the next connection attempt is scheduled and can be queried.
/// 2. `PendingConnection`: This state means that a connection attempt to the peer is currently
/// in progress.
/// 3. `Connected`: This state signals that the peer is currently connected.
///
/// The protocol will attempt to establish a connection to a `NotConnected` peer after a set delay,
/// specified by `DIALING_INTERVAL_IN_SECS`, to prevent multiple simultaneous connection attempts
/// to offline peers. This delay not only conserves resources, but also reduces the amount of
/// log output.
///
/// ## Comments
///
/// The protocol will establish one or two connections between each pair of reserved peers.
///
/// IMPORTANT NOTE: For the maintenance of a persistent connection, both peers should have each
/// other in their `reserved peers set`. This is necessary because if only one peer has the other
/// in its `reserved peers set`, regular connection attempts will occur, but these connections will
/// be dismissed on the other side due to the `KeepAlive` policy.
///
#[derive(Debug)]
pub struct Behaviour<PeerSource> {
    config: Config,

    known_peers: HashMap<PeerId, PeerDecision>,

    peer_source: PeerSource,

    connection_candidates: Vec<PeerAddress>
}

impl<PeerSource> Behaviour<PeerSource> {
    /// Creates a new `Behaviour` with a predefined set of reserved peers.
    pub fn new(config: Config, peer_source: PeerSource) -> Self {
        Self {
            config,
            known_peers: HashMap::new(),
            peer_source,
            connection_candidates: Vec::new(),
        }
    }

    /// Create a connection handler for the protocol.
    #[inline]
    fn new_connection_handler(&self, peer_id: &PeerId) -> Handler {
        let current_decision = if let Some(state) = self.known_peers.get(peer_id) {
            !matches!(state, PeerDecision::NotInterested)
        } else {
            warn!(%peer_id, "Connected peers protocol: cannot find peer state.");
            false
        };
        Handler::new(self.config.protocol_name, current_decision)
    }
}

impl<PeerSource: PeerAddressSource +'static> NetworkBehaviour for Behaviour<PeerSource> {
    type ConnectionHandler = Handler;
    type OutEvent = Event;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.new_connection_handler(&peer_id))
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer_id: PeerId,
        _: &Multiaddr,
        _: Endpoint,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(self.new_connection_handler(&peer_id))
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match event {
            FromSwarm::ConnectionEstablished(ConnectionEstablished { peer_id, .. }) => {
                if let Some(decision) = self.known_peers.get_mut(&peer_id) {
                    if let PeerDecision::PendingConnection { state } = decision {
                        state.connection_status = ConnectionStatus::Connected;

                        debug!(peer_id=%state.peer_id, "Reserved peer connected.");
                        // TODO
                    }
                }
            }
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                remaining_established,
                ..
            }) => {
                if let Some(decision) = self.known_peers.get_mut(&peer_id) {
                    if remaining_established == 0 {
                        *decision = PeerDecision::NotInterested;

                        debug!(%peer_id, "Reserved peer disconnected."); // TODO
                    }
                }
            }
            FromSwarm::DialFailure(DialFailure { peer_id, .. }) => {
                if let Some(peer_id) = peer_id {
                    if let Some(decision) = self.known_peers.get_mut(&peer_id) {
                        *decision = PeerDecision::NotInterested;

                        debug!(%peer_id, "Reserved peer dialing failed."); // TODO
                    }
                }
            }
            FromSwarm::AddressChange(_)
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
        while let Some(peer_addreess) = self.connection_candidates.pop() {
            // trace!(?state, "Reserved peer state."); // TODO

            if let ConnectionStatus::NotConnected { scheduled_at } = state.connection_status {
                if Instant::now() > scheduled_at {
                    state.connection_status = ConnectionStatus::PendingConnection;

                    debug!(peer_id=%state.peer_id, "Dialing the reserved peer....");

                    let dial_opts =
                        DialOpts::peer_id(state.peer_id).addresses(vec![state.address.clone()]);

                    return Poll::Ready(ToSwarm::Dial {
                        opts: dial_opts.build(),
                    });
                }
            }
        }

        // New dial condiates
        let connected_peers: u32 = self
            .known_peers
            .iter()
            .filter_map(|(peer_id, state)| {
                if *state == PeerDecision::PermanentConnection {
                    Some(1)
                } else {
                    None
                }
            })
            .sum();

        if connected_peers < self.config.target_connected_peers{
            let mut peer_addresses = self.peer_source.peer_addresses(self.config.next_peer_batch_size);

            self.connection_candidates.extend_from_slice(&peer_addresses);
        }

        // for (_, state) in self.reserved_peers_state.iter_mut() {
        //     trace!(?state, "Reserved peer state.");
        //
        //     if let ConnectionStatus::NotConnected { scheduled_at } = state.connection_status {
        //         if Instant::now() > scheduled_at {
        //             state.connection_status = ConnectionStatus::PendingConnection;
        //
        //             debug!(peer_id=%state.peer_id, "Dialing the reserved peer....");
        //
        //             let dial_opts =
        //                 DialOpts::peer_id(state.peer_id).addresses(vec![state.address.clone()]);
        //
        //             return Poll::Ready(ToSwarm::Dial {
        //                 opts: dial_opts.build(),
        //             });
        //         }
        //     }
        // }

        // TODO: waker
        Poll::Pending
    }
}
