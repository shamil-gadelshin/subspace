pub(crate) mod custom_record_store;
pub(crate) mod persistent_parameters;

use crate::create::{RelayConfiguration, ValueGetter};
use crate::request_responses::{
    Event as RequestResponseEvent, ProtocolConfig as RequestResponseConfig,
    RequestResponseHandlerRunner, RequestResponseInstanceConfig, RequestResponsesBehaviour,
};
use crate::shared::IdendityHash;
use custom_record_store::CustomRecordStore;
use derive_more::From;
use libp2p::gossipsub::{Gossipsub, GossipsubConfig, GossipsubEvent, MessageAuthenticity};
use libp2p::identify::{Identify, IdentifyConfig, IdentifyEvent};
use libp2p::kad::{Kademlia, KademliaConfig, KademliaEvent};
use libp2p::ping::{Ping, PingEvent};
use libp2p::relay::v2::client::{Client as RelayClient, Event as RelayClientEvent};
use libp2p::relay::v2::relay::{Event as RelayEvent, Relay};
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{Multiaddr, NetworkBehaviour, PeerId};

pub(crate) struct BehaviorConfig {
    /// Identity keypair of a node used for authenticated connections.
    pub(crate) peer_id: PeerId,
    /// Nodes to connect to on creation.
    pub(crate) bootstrap_nodes: Vec<(PeerId, Multiaddr)>,
    /// The configuration for the [`Identify`] behaviour.
    pub(crate) identify: IdentifyConfig,
    /// The configuration for the [`Kademlia`] behaviour.
    pub(crate) kademlia: KademliaConfig,
    /// The configuration for the [`Gossipsub`] behaviour.
    pub(crate) gossipsub: GossipsubConfig,
    /// Externally provided implementation of value getter for Kademlia DHT,
    pub(crate) value_getter: ValueGetter,
    /// The configuration for the [`RequestResponsesBehaviour`] protocol.
    pub(crate) pieces_by_range_protocol_config: RequestResponseConfig,
    /// The pieces-by-range request handler.
    pub(crate) pieces_by_range_request_handler: Box<dyn RequestResponseHandlerRunner + Send>,
    /// Defines relay circuits configuration.
    pub(crate) relay_config: RelayConfiguration,
}

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "Event")]
#[behaviour(event_process = false)]
pub(crate) struct Behavior {
    pub(crate) identify: Identify,
    pub(crate) kademlia: Kademlia<CustomRecordStore, IdendityHash>,
    pub(crate) gossipsub: Gossipsub,
    pub(crate) ping: Ping,
    pub(crate) request_response: RequestResponsesBehaviour,
    pub(crate) relay: Toggle<Relay>,
    pub(crate) relay_client: Toggle<RelayClient>,
}

impl Behavior {
    pub(crate) fn new(config: BehaviorConfig, relay_client: Option<RelayClient>) -> Self {
        let kademlia = {
            let store = CustomRecordStore::new(config.value_getter);
            let mut kademlia =
                Kademlia::<_, IdendityHash>::with_config(config.peer_id, store, config.kademlia);

            for (peer_id, address) in config.bootstrap_nodes {
                kademlia.add_address(&peer_id, address);
            }

            kademlia
        };

        let gossipsub = Gossipsub::new(
            // TODO: Do we want message signing?
            MessageAuthenticity::Anonymous,
            config.gossipsub,
        )
        .expect("Correct configuration");

        let relay = config
            .relay_config
            .server_relay_settings()
            .map(|settings| Relay::new(config.peer_id, settings.to_relay_config()))
            .into();

        Self {
            identify: Identify::new(config.identify),
            kademlia,
            gossipsub,
            ping: Ping::default(),
            request_response: RequestResponsesBehaviour::new(
                vec![RequestResponseInstanceConfig {
                    config: config.pieces_by_range_protocol_config,
                    handler: config.pieces_by_range_request_handler,
                }]
                .into_iter(),
            )
            //TODO: Convert to an error.
            .expect("RequestResponse protocols registration failed."),
            relay,
            relay_client: relay_client.into(),
        }
    }
}

#[derive(Debug, From)]
pub(crate) enum Event {
    Identify(IdentifyEvent),
    Kademlia(KademliaEvent),
    Gossipsub(GossipsubEvent),
    Ping(PingEvent),
    RequestResponse(RequestResponseEvent),
    Relay(RelayEvent),
    RelayClient(RelayClientEvent),
}
