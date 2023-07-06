mod handler;

use futures::FutureExt;
use futures_timer::Delay;
use handler::Handler;
use libp2p::core::{Endpoint, Multiaddr};
use libp2p::swarm::behaviour::{ConnectionEstablished, FromSwarm};
use libp2p::swarm::dial_opts::DialOpts;
use libp2p::swarm::{
    ConnectionClosed, ConnectionDenied, ConnectionId, DialFailure, KeepAlive, NetworkBehaviour,
    NotifyHandler, PollParameters, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::PeerId;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ops::Add;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tracing::{debug, trace};

// TODO: remove peer-info
// TODO: fix comments and other strings
// TODO: remove old reconnections

use crate::utils::PeerAddress;

pub trait PeerAddressSource {
    fn peer_addresses(&self, batch_size: u32) -> Vec<PeerAddress>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PeerDecision {
    //TODO: add delay for new candidates
    PendingConnection { peer_address: PeerAddress },
    //TODO: add timeout for the decision
    PendingDecision { until: Instant },
    PermanentConnection,
    NotInterested,
}

/// Reserved peers protocol configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Protocol name.
    pub protocol_name: &'static [u8],
    /// Interval between new dialing attempts.
    pub dialing_interval: Duration,
    /// Number of connected peers that protocol will maintain.
    pub target_connected_peers: u32,
    /// We dial peers using this batch size.
    pub dialing_peer_batch_size: u32,
    /// Time interval reserved for a decision about connections.
    /// It also affects keep-alive interval.
    pub decision_timeout: Duration,
}

const CONNECTED_PEERS_PROTOCOL_NAME: &[u8] = b"/subspace/connected-peers/1.0.0";
impl Default for Config {
    fn default() -> Self {
        Self {
            protocol_name: CONNECTED_PEERS_PROTOCOL_NAME,
            dialing_interval: Duration::from_secs(3),
            target_connected_peers: 30,
            dialing_peer_batch_size: 5,
            decision_timeout: Duration::from_secs(10),
        }
    }
}

/// Connected-peers protocol event.
#[derive(Debug, Clone)]
pub enum Event {
    /// We need a new batch of peer addresses from the swarm.
    NewDialingCandidatesRequested,
}

//TODO: rename
/// Defines .....
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerDecisionChange {
    peer_id: PeerId,
    keep_alive: KeepAlive,
}

#[derive(Debug)]
pub struct Behaviour {
    config: Config,

    known_peers: HashMap<PeerId, PeerDecision>,

    // TODO: remove this one
    connection_candidates: Vec<PeerAddress>,

    peer_decision_changes: Vec<PeerDecisionChange>,

    dialing_delay: Delay,

    peer_source: Vec<PeerAddress>,
}

impl Behaviour {
    /// Creates a new `Behaviour`.
    pub fn new(config: Config) -> Self {
        let dialing_delay = Delay::new(config.dialing_interval);
        Self {
            config,
            known_peers: HashMap::new(),
            connection_candidates: Vec::new(),
            peer_decision_changes: Vec::new(),
            dialing_delay,
            peer_source: Vec::new(),
        }
    }

    /// Create a connection handler for the protocol.
    #[inline]
    fn new_connection_handler(&self, peer_id: &PeerId) -> Handler {
        // TODO: review
        let default_keep_alive_until =
            KeepAlive::Until(Instant::now().add(self.config.decision_timeout));
        let keep_alive = if let Some(decision) = self.known_peers.get(peer_id) {
            match decision {
                PeerDecision::PendingConnection { .. } => default_keep_alive_until,
                PeerDecision::PendingDecision { until } => KeepAlive::Until(*until),
                PeerDecision::PermanentConnection => KeepAlive::Yes,
                PeerDecision::NotInterested => KeepAlive::No,
            }
        } else {
            default_keep_alive_until
        };

        Handler::new(self.config.protocol_name, keep_alive)
    }

    /// Specifies the whether we should keep connections to the peer alive.
    pub fn update_keep_alive_status(&mut self, peer_id: PeerId, keep_alive: bool) {
        // TODO: review this
        //TODO: remove decision?
        let (decision, keep_alive) = if keep_alive {
            if self.permanently_connected_peers() < self.config.target_connected_peers {
                trace!(%peer_id, %keep_alive, "Insufficient number of connected peers.");

                (PeerDecision::PermanentConnection, KeepAlive::Yes)
            } else {
                trace!(%peer_id, %keep_alive, "Target number of connected peers reached.");

                (PeerDecision::NotInterested, KeepAlive::No)
            }
        } else {
            (PeerDecision::NotInterested, KeepAlive::No)
        };

        self.known_peers.insert(peer_id, decision);
        self.peer_decision_changes.push(PeerDecisionChange {
            peer_id,
            keep_alive,
        });
    }

    /// Calculates the current number of permanently connected peers.
    fn permanently_connected_peers(&self) -> u32 {
        self.known_peers
            .iter()
            .filter_map(|(_, state)| {
                if *state == PeerDecision::PermanentConnection {
                    Some(1u32)
                } else {
                    None
                }
            })
            .sum()
    }

    /// Adds peer addresses for internal cache. We use these addresses to dial peers for maintaining
    /// target connection number.
    pub fn add_peers_to_dial(&mut self, peers: Vec<PeerAddress>) {
        self.peer_source.extend_from_slice(&peers);
    }
}

impl NetworkBehaviour for Behaviour {
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
                if let Entry::Vacant(entry) = self.known_peers.entry(peer_id) {
                    entry.insert(PeerDecision::PendingDecision {
                        until: Instant::now().add(self.config.decision_timeout),
                    });

                    trace!(%peer_id, "Pending peer decision...");
                }
            }
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                remaining_established,
                ..
            }) => {
                if remaining_established == 0 {
                    let old_peer_decision = self.known_peers.remove(&peer_id);

                    if old_peer_decision.is_some() {
                        trace!(%peer_id, ?old_peer_decision, "Known peer disconnected.");
                    }
                }
            }
            FromSwarm::DialFailure(DialFailure { peer_id, .. }) => {
                // TODO: what will happen with existing connections
                if let Some(peer_id) = peer_id {
                    let old_peer_decision = self.known_peers.remove(&peer_id);

                    if old_peer_decision.is_some() {
                        debug!(%peer_id, ?old_peer_decision, "Dialing error to known peer.");
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
        cx: &mut Context<'_>,
        _: &mut impl PollParameters,
    ) -> Poll<ToSwarm<Self::OutEvent, THandlerInEvent<Self>>> {
        // Notify handlers about received connection decision.
        if let Some(change) = self.peer_decision_changes.pop() {
            return Poll::Ready(ToSwarm::NotifyHandler {
                peer_id: change.peer_id,
                handler: NotifyHandler::Any,
                event: change.keep_alive,
            });
        }

        // Check decision statuses.
        for (peer_id, decision) in self.known_peers.iter_mut() {
            trace!(%peer_id, ?decision, "Peer decisions for connected peers protocol.");

            match decision.clone() {
                PeerDecision::PendingConnection {
                    peer_address: (peer_id, address),
                } => {
                    *decision = PeerDecision::PendingDecision {
                        until: Instant::now().add(self.config.decision_timeout),
                    };

                    debug!(%peer_id, "Dialing a new peer.");

                    let dial_opts = DialOpts::peer_id(peer_id).addresses(vec![address]);

                    return Poll::Ready(ToSwarm::Dial {
                        opts: dial_opts.build(),
                    });
                }
                PeerDecision::PendingDecision { until } => {
                    if until < Instant::now() {
                        *decision = PeerDecision::NotInterested; // timeout
                    }
                }
                PeerDecision::PermanentConnection | PeerDecision::NotInterested => {
                    // Decision is made - no action.
                }
            }
        }

        // TODO:  > connection timeout ?
        match self.dialing_delay.poll_unpin(cx) {
            Poll::Pending => {}
            Poll::Ready(()) => {
                self.dialing_delay.reset(self.config.dialing_interval);

                if self.peer_source.is_empty() {
                    trace!("Requesting new peers for connected-peers protocol....");

                    return Poll::Ready(ToSwarm::GenerateEvent(
                        Event::NewDialingCandidatesRequested,
                    ));
                }

                // New dial candidates
                if self.permanently_connected_peers() < self.config.target_connected_peers {
                    let range = 0..(self.config.dialing_peer_batch_size as usize)
                        .min(self.peer_source.len());

                    let peer_addresses = self.peer_source.drain(range).collect::<Vec<_>>();

                    self.connection_candidates
                        .extend_from_slice(&peer_addresses);
                }

                while let Some(peer_address) = self.connection_candidates.pop() {
                    self.known_peers.entry(peer_address.0).or_insert_with(|| {
                        PeerDecision::PendingConnection {
                            peer_address: peer_address.clone(),
                        }
                    });
                }
            }
        }

        // TODO: waker
        Poll::Pending
    }
}
