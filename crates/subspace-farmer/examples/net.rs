mod sc_network;

use crate::sc_network::peer_info::{PeerInfoBehaviour, PeerInfoEvent};
use futures::StreamExt;
use libp2p::kad::record::store::MemoryStore;
use libp2p::kad::{GetClosestPeersError, Kademlia, KademliaConfig, KademliaEvent, QueryResult};
use libp2p::swarm::{Swarm, SwarmEvent};
use libp2p::{
    core, dns, identity, noise, tcp, websocket, yamux, Multiaddr, NetworkBehaviour, PeerId,
    Transport,
};
use std::error::Error;
use std::str::FromStr;
use std::time::Duration;

const KADEMLIA_PROTOCOL: &[u8] = b"/subspace/kad";

#[derive(NetworkBehaviour)]
#[behaviour(out_event = "ComposedEvent")]
#[behaviour(event_process = false)]
struct ComposedBehaviour {
    peer_info_behavior: PeerInfoBehaviour,
    kademlia: Kademlia<MemoryStore>,
}

#[derive(Debug)]
enum ComposedEvent {
    PeerInfo(PeerInfoEvent),
    Kademlia(KademliaEvent),
}

impl From<PeerInfoEvent> for ComposedEvent {
    fn from(event: PeerInfoEvent) -> Self {
        ComposedEvent::PeerInfo(event)
    }
}

impl From<KademliaEvent> for ComposedEvent {
    fn from(event: KademliaEvent) -> Self {
        ComposedEvent::Kademlia(event)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    // Create a random key for ourselves.
    let local_keypair = identity::Keypair::generate_ed25519();
    let local_public_key = local_keypair.public();
    let local_peer_id = local_public_key.to_peer_id();

    let transport = {
        let transport = {
            let tcp = tcp::TcpConfig::new().nodelay(true);
            let dns_tcp = dns::DnsConfig::system(tcp).await?;
            let ws_dns_tcp = websocket::WsConfig::new(dns_tcp.clone());
            dns_tcp.or_transport(ws_dns_tcp)
        };

        let noise_keys = noise::Keypair::<noise::X25519Spec>::new()
            .into_authentic(&local_keypair)
            .expect("Signing libp2p-noise static DH keypair failed.");

        transport
            .upgrade(core::upgrade::Version::V1)
            .authenticate(noise::NoiseConfig::xx(noise_keys).into_authenticated())
            .multiplex(yamux::YamuxConfig::default())
            .timeout(std::time::Duration::from_secs(20))
            .boxed()
    };

    let peer_info_behavior = PeerInfoBehaviour::new("Hello".to_string(), local_public_key);

    let kademlia = {
        let mut config = KademliaConfig::default();
        config.set_protocol_name(KADEMLIA_PROTOCOL);
        config.set_query_timeout(Duration::from_secs(5 * 60));
        let store = MemoryStore::new(local_peer_id);
        let mut kademlia = Kademlia::with_config(local_peer_id, store, config);

        // Add the bootnodes to the local routing table.
        kademlia.add_address(
            &PeerId::from_str("12D3KooWEyoppNCUx8Yx66oV9fJnriXwCcXwDDUA2kj6vnc6iDEp")?,
            Multiaddr::from_str(
                "/ip4/127.0.0.1/tcp/30335/p2p/12D3KooWEyoppNCUx8Yx66oV9fJnriXwCcXwDDUA2kj6vnc6iDEp",
            )?,
        );

        kademlia
    };

    let behavior = ComposedBehaviour {
        peer_info_behavior,
        kademlia,
    };

    let mut swarm = Swarm::new(transport, behavior, local_peer_id);

    let to_search: PeerId = identity::Keypair::generate_ed25519().public().into();

    println!("Searching for the closest peers to {:?}", to_search);
    swarm.behaviour_mut().kademlia.get_closest_peers(to_search);

    loop {
        let event = swarm.select_next_some().await;
        if let SwarmEvent::Behaviour(ComposedEvent::Kademlia(
            KademliaEvent::OutboundQueryCompleted {
                result: QueryResult::GetClosestPeers(result),
                ..
            },
        )) = event
        {
            match result {
                Ok(ok) => {
                    if !ok.peers.is_empty() {
                        println!("Query finished with closest peers: {:#?}", ok.peers)
                    } else {
                        // The example is considered failed as there
                        // should always be at least 1 reachable peer.
                        println!("Query finished with no closest peers.")
                    }
                }
                Err(GetClosestPeersError::Timeout { peers, .. }) => {
                    if !peers.is_empty() {
                        println!("Query timed out with closest peers: {:#?}", peers)
                    } else {
                        // The example is considered failed as there
                        // should always be at least 1 reachable peer.
                        println!("Query timed out with no closest peers.");
                    }
                }
            };
        }
    }
}
