use async_trait::async_trait;
use futures::channel::mpsc::{self, UnboundedSender};
use futures::{select, FutureExt, SinkExt, StreamExt};
use libp2p::{Multiaddr, PeerId};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::trace;

//TODO
const PEER_CACHE_SIZE: usize = 100;

//TODO
pub type NetworkingParameterPersistenceHandler =
    Arc<dyn PersistentNetworkingParametersManager + Send + Sync + 'static>;

//TODO:
#[derive(Debug)]
pub struct NetworkingParametersCache {
    pub known_peers: LruCache<PeerId, HashSet<Multiaddr>>,
}

impl Clone for NetworkingParametersCache {
    fn clone(&self) -> Self {
        let mut known_peers = LruCache::new(self.known_peers.cap());

        for (peer_id, addresses) in self.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self { known_peers }
    }
}

impl NetworkingParametersCache {
    fn new(cache_cap: usize) -> Self {
        Self {
            known_peers: LruCache::new(cache_cap),
        }
    }
    fn add_known_peer(&mut self, peer_id: PeerId, addr_set: HashSet<Multiaddr>) {
        if let Some(addresses) = self.known_peers.get_mut(&peer_id) {
            *addresses = addresses.union(&addr_set).cloned().collect()
        } else {
            self.known_peers.push(peer_id, addr_set);
        }
    }

    fn get_known_peer_addresses(&self, peer_number: usize) -> Vec<(PeerId, Multiaddr)> {
        self.known_peers
            .iter()
            .take(peer_number)
            .map(|(peer_id, addresses)| addresses.iter().map(|addr| (*peer_id, addr.clone())))
            .flatten()
            .collect()
    }
}

#[async_trait]
pub trait NetworkingParametersManager {
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>);
}

// TODO:  save-load params?
pub trait PersistentNetworkingParametersManager: Send {
    fn load(&self) -> anyhow::Result<NetworkingParametersCache>;
    fn save(&self, params: &NetworkingParametersCache) -> anyhow::Result<()>;
}

pub struct NetworkingDataManager<P: PersistentNetworkingParametersManager> {
    stop_handle: JoinHandle<()>,
    tx: UnboundedSender<(PeerId, HashSet<Multiaddr>)>, //TODO
    initial_bootstrap_addresses: Vec<(PeerId, Multiaddr)>,
    _persistence_marker: PhantomData<P>,
}

impl<P: PersistentNetworkingParametersManager> Drop for NetworkingDataManager<P> {
    fn drop(&mut self) {
        self.stop_handle.abort();
    }
}

impl<P: PersistentNetworkingParametersManager + Clone + Send + 'static> NetworkingDataManager<P> {
    pub fn new(
        network_parameters_persistence_handler: NetworkingParameterPersistenceHandler,
    ) -> NetworkingDataManager<P> {
        let (tx, mut rx) = mpsc::unbounded();

        let networking_params = network_parameters_persistence_handler
            .load()
            .unwrap_or(NetworkingParametersCache::new(PEER_CACHE_SIZE));
        let initial_cache: NetworkingParametersCache = networking_params.into();

        const INITIAL_BOOTSTRAP_ADDRESS_NUMBER: usize = 100; //TODO
        let delay_duration = Duration::from_secs(5); //TODO
        let initial_bootstrap_addresses =
            initial_cache.get_known_peer_addresses(INITIAL_BOOTSTRAP_ADDRESS_NUMBER);

        let stop_handle = tokio::spawn(async move {
            let mut params_cache = initial_cache;
            let mut delay = sleep(delay_duration).boxed().fuse();
            loop {
                select! {
                    _ = delay => {
                        if let Err(err) = network_parameters_persistence_handler.save(
                            &params_cache.clone().into()
                        ) {
                            trace!(error=%err, "Error on saving network parameters");
                        }

                        // restart the delay future
                        delay = sleep(delay_duration).boxed().fuse();
                    },
                    data = rx.next() => {
                        if let Some((peer_id, addr_set)) = data {
                            println!("New data: {:?}", (peer_id, &addr_set));

                            params_cache.add_known_peer(peer_id, addr_set); //TODO

                            println!("Known peers so far: {:?}", params_cache);
                        }
                    }
                }
            }
        });

        NetworkingDataManager {
            tx,
            stop_handle,
            initial_bootstrap_addresses,
            _persistence_marker: PhantomData,
        }
    }

    //TODO: comment, convert from P2p-address ??
    pub fn initial_bootstrap_addresses(&self) -> Vec<(PeerId, Multiaddr)> {
        self.initial_bootstrap_addresses.clone()
    }
}

#[async_trait]
impl<P: PersistentNetworkingParametersManager + Send> NetworkingParametersManager
    for NetworkingDataManager<P>
{
    async fn add_known_peer(&mut self, peer_id: PeerId, addresses: Vec<Multiaddr>) {
        let addr_set = addresses.iter().cloned().collect::<HashSet<_>>();

        let _ = self.tx.send((peer_id, addr_set)).await; // TODO
    }
}

// Helper struct for JsonNetworkingPersistence
#[derive(Default, Debug, Serialize, Deserialize)]
struct JsonNetworkingParameters {
    pub known_peers: HashMap<PeerId, HashSet<Multiaddr>>,
}

impl From<&NetworkingParametersCache> for JsonNetworkingParameters {
    fn from(cache: &NetworkingParametersCache) -> Self {
        Self {
            known_peers: cache
                .known_peers
                .iter()
                .map(|(peer_id, addresses)| (*peer_id, addresses.clone()))
                .collect(),
        }
    }
}

impl From<JsonNetworkingParameters> for NetworkingParametersCache {
    fn from(params: JsonNetworkingParameters) -> Self {
        let mut known_peers = LruCache::<PeerId, HashSet<Multiaddr>>::new(PEER_CACHE_SIZE);

        for (peer_id, addresses) in params.known_peers.iter() {
            known_peers.push(*peer_id, addresses.clone());
        }

        Self {
            known_peers, //TODO: iter?
        }
    }
}

//TODO: empty saver, result errors, parameters?
#[derive(Clone)]
pub struct JsonNetworkingPersistence {
    pub path: String, //TODO private
}

impl PersistentNetworkingParametersManager for JsonNetworkingPersistence {
    fn load(&self) -> anyhow::Result<NetworkingParametersCache> {
        let data = fs::read(&self.path)?; //.expect("Unable to read file"); //TODO

        let result: JsonNetworkingParameters =
            serde_json::from_slice(&data).expect("Cannot serialize networking parameters to JSON"); //TODO

        println!("Networking parameters loaded");

        Ok(result.into())
    }

    fn save(&self, params: &NetworkingParametersCache) -> anyhow::Result<()> {
        let params: JsonNetworkingParameters = params.into();
        let data =
            serde_json::to_string(&params).expect("Cannot serialize networking parameters to JSON"); //TODO

        fs::write(&self.path, data).expect("Unable to write file"); //TODO
        println!("Networking parameters saved");

        Ok(())
    }
}

#[derive(Clone)]
pub struct NetworkPersistenceStub;

impl PersistentNetworkingParametersManager for NetworkPersistenceStub {
    fn load(&self) -> anyhow::Result<NetworkingParametersCache> {
        trace!("Default network parameters used");

        Ok(NetworkingParametersCache::new(PEER_CACHE_SIZE))
    }

    fn save(&self, _: &NetworkingParametersCache) -> anyhow::Result<()> {
        trace!("Network parameters saving skipped.");

        Ok(())
    }
}
